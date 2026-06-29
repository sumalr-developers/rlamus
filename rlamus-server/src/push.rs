use std::borrow::Cow;

use apns_h2::{DefaultNotificationBuilder, NotificationBuilder, NotificationOptions, PushType};

use crate::task::Task;

pub async fn apn_completion(
    task: &Task,
    client: &apns_h2::Client,
    device_token: impl AsRef<str>,
) -> Result<(), apns_h2::Error> {
    match task.state.clone() {
        crate::task::TaskState::Init => {}
        crate::task::TaskState::Scraping => {}
        crate::task::TaskState::Summarizing { title } => {}
        crate::task::TaskState::Done { title, summary: _ } => {
            let mut builder = DefaultNotificationBuilder::new()
                .title_loc_key("TASK_DONE")
                .sound("default")
                .badge(1);
            let mut args = Vec::new();
            if let Some(title) = title.as_ref() {
                args.push(Cow::Borrowed(title.as_str()));
                builder = builder.subtitle_loc_key("TASK_TITLED_X_DONE");
            } else {
                builder = builder.subtitle_loc_key("RECENT_TASK_DONE");
            }
            builder = builder.subtitle_loc_args(&args);
            client
                .send(builder.build(
                    device_token.as_ref(),
                    NotificationOptions {
                        apns_push_type: Some(PushType::Alert),
                        ..Default::default()
                    },
                ))
                .await?;
        }
        crate::task::TaskState::Failed(smol_str) => {}
    }
    Ok(())
}
