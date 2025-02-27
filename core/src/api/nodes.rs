use crate::{invalidate_query, util::MaybeUndefined};

use sd_prisma::prisma::{instance, location};

use rspc::{alpha::AlphaRouter, ErrorCode};
use serde::Deserialize;
use specta::Type;
use tracing::error;
use uuid::Uuid;

use super::{locations::ExplorerItem, utils::library, Ctx, R};

pub(crate) fn mount() -> AlphaRouter<Ctx> {
	R.router()
		.procedure("edit", {
			#[derive(Deserialize, Type)]
			pub struct ChangeNodeNameArgs {
				pub name: Option<String>,
				pub p2p_port: MaybeUndefined<u16>,
				pub p2p_enabled: Option<bool>,
				pub image_labeler_version: Option<String>,
			}
			R.mutation(|node, args: ChangeNodeNameArgs| async move {
				if let Some(name) = &args.name {
					if name.is_empty() || name.len() > 250 {
						return Err(rspc::Error::new(
							ErrorCode::BadRequest,
							"invalid node name".into(),
						));
					}
				}

				let does_p2p_need_refresh =
					args.p2p_enabled.is_some() || args.p2p_port.is_defined();

				#[cfg(feature = "ai")]
				let mut new_model = None;

				node.config
					.write(|config| {
						if let Some(name) = args.name {
							config.name = name;
						}

						config.p2p.enabled = args.p2p_enabled.unwrap_or(config.p2p.enabled);

						if let Some(v) = args.p2p_port.into() {
							config.p2p.port = v;
						}

						#[cfg(feature = "ai")]
						if let Some(version) = args.image_labeler_version {
							if config
								.image_labeler_version
								.as_ref()
								.map(|node_version| version != *node_version)
								.unwrap_or(true)
							{
								new_model = sd_ai::image_labeler::YoloV8::model(Some(&version))
									.map_err(|e| {
										error!(
										"Failed to crate image_detection model: '{}'; Error: {e:#?}",
										&version,
									);
									})
									.ok();
								if new_model.is_some() {
									config.image_labeler_version = Some(version);
								}
							}
						}
					})
					.await
					.map_err(|err| {
						error!("Failed to write config: {}", err);
						rspc::Error::new(
							ErrorCode::InternalServerError,
							"error updating config".into(),
						)
					})?;

				// If a P2P config was modified reload it
				if does_p2p_need_refresh {
					node.p2p
						.manager
						.update_config(node.config.get().await.p2p.clone())
						.await;
				}

				invalidate_query!(node; node, "nodeState");

				#[cfg(feature = "ai")]
				{
					use super::notifications::{NotificationData, NotificationKind};

					if let Some(model) = new_model {
						let version = model.version().to_string();
						tokio::spawn(async move {
							let notification = if let Err(e) =
								node.image_labeller.change_model(model).await
							{
								NotificationData {
									title: String::from("Failed to change image detection model"),
									content: format!("Error: {e}"),
									kind: NotificationKind::Error,
								}
							} else {
								NotificationData {
									title: String::from("Model download completed"),
									content: format!("Sucessfuly loaded model: {version}"),
									kind: NotificationKind::Success,
								}
							};

							node.emit_notification(notification, None).await;
						});
					}
				}

				Ok(())
			})
		})
		// TODO: add pagination!! and maybe ordering etc
		.procedure("listLocations", {
			R.with2(library())
				// TODO: I don't like this. `node_id` should probs be a machine hash or something cause `node_id` is dynamic in the context of P2P and what does it mean for removable media to be owned by a node?
				.query(|(_, library), node_id: Option<Uuid>| async move {
					// Be aware multiple instances can exist on a single node. This is generally an edge case but it's possible.
					let instances = library
						.db
						.instance()
						.find_many(vec![node_id
							.map(|id| instance::node_id::equals(id.as_bytes().to_vec()))
							.unwrap_or(instance::id::equals(
								library.config().await.instance_id,
							))])
						.exec()
						.await?;

					Ok(library
						.db
						.location()
						.find_many(
							instances
								.into_iter()
								.map(|i| location::instance_id::equals(Some(i.id)))
								.collect(),
						)
						.exec()
						.await?
						.into_iter()
						.map(|location| ExplorerItem::Location { item: location })
						.collect::<Vec<_>>())
				})
		})
		.procedure("updateThumbnailerPreferences", {
			#[derive(Deserialize, Type)]
			pub struct UpdateThumbnailerPreferences {
				pub background_processing_percentage: u8, // 0-100
			}
			R.mutation(
				|node,
				 UpdateThumbnailerPreferences {
				     background_processing_percentage,
				 }: UpdateThumbnailerPreferences| async move {
					node.config
						.update_preferences(|preferences| {
							preferences
								.thumbnailer
								.set_background_processing_percentage(
									background_processing_percentage,
								);
						})
						.await
						.map_err(|e| {
							error!("failed to update thumbnailer preferences: {e:#?}");
							rspc::Error::with_cause(
								ErrorCode::InternalServerError,
								"Failed to update thumbnailer preferences".to_string(),
								e,
							)
						})
				},
			)
		})
}
