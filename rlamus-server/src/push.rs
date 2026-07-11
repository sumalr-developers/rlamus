use std::borrow::Cow;

use apns_h2::{DefaultNotificationBuilder, NotificationBuilder, NotificationOptions, PushType};

use crate::task::Task;

pub async fn apn_state_change(
    task: &Task,
    client: &apns_h2::Client,
    device_token: impl AsRef<str>,
    topic: Option<impl AsRef<str>>,
) -> Result<(), apns_h2::Error> {
    match task.state.clone() {
        crate::task::TaskState::Init => {}
        crate::task::TaskState::Scraping => {}
        crate::task::TaskState::Summarizing { title: _ } => {}
        crate::task::TaskState::Embedding { title, summary: _ } => {
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

            let mut payload = builder.build(
                device_token.as_ref(),
                NotificationOptions {
                    apns_push_type: Some(PushType::Alert),
                    apns_topic: topic.as_ref().map(|it| it.as_ref()),
                    ..Default::default()
                },
            );
            payload
                .add_custom_data("task-id", &task.id.to_string())
                .unwrap();

            client.send(payload).await?;
        }
        crate::task::TaskState::Done {
            title: _,
            summary: _,
            embedding: _,
            embedding_model: _,
        } => {}
        crate::task::TaskState::Failed(_) => {}
    }
    Ok(())
}
