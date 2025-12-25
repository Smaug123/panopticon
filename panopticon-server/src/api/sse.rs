use std::convert::Infallible;
use std::pin::Pin;
use std::time::Duration;

use axum::extract::{Path, State};
use axum::response::sse::{Event, Sse};
use futures::Stream;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;

use crate::api::error::ApiError;
use crate::db::reviews;
use crate::domain::ids::ReviewId;
use crate::domain::review::ReviewStatus;
use crate::AppState;
use crate::ReviewUpdateKind;

type SseStream = Pin<Box<dyn Stream<Item = Result<Event, Infallible>> + Send>>;

/// SSE endpoint for streaming review updates.
pub async fn review_stream(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Sse<SseStream>, ApiError> {
    let review_id = ReviewId::new(id);

    // Verify review exists
    let review = reviews::get_by_id(&state.db, review_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("Review not found".to_string()))?;

    // If review is already completed, send the results immediately
    if matches!(review.status, ReviewStatus::Completed { .. }) {
        let stream: SseStream = Box::pin(futures::stream::once(async move {
            let data = serde_json::json!({
                "type": "complete",
                "results": review.results,
            });
            Ok::<_, Infallible>(Event::default().data(data.to_string()))
        }));

        return Ok(Sse::new(stream).keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("ping"),
        ));
    }

    // Subscribe to updates
    let rx = state.review_updates.subscribe();
    let stream: SseStream = Box::pin(BroadcastStream::new(rx).filter_map(
        move |result| match result {
            Ok(update) if update.review_id == review_id => {
                let data = match update.kind {
                    ReviewUpdateKind::Chunk { text } => serde_json::json!({
                        "type": "chunk",
                        "prompt_name": update.prompt_name,
                        "text": text,
                    }),
                    ReviewUpdateKind::PromptComplete => serde_json::json!({
                        "type": "prompt_complete",
                        "prompt_name": update.prompt_name,
                    }),
                    ReviewUpdateKind::ReviewComplete => serde_json::json!({
                        "type": "complete",
                    }),
                };
                Some(Ok::<_, Infallible>(Event::default().data(data.to_string())))
            }
            _ => None,
        },
    ));

    Ok(Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    ))
}
