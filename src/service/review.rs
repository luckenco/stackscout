//! Rolling review ownership. Semantic comparison runs outside the publication lock.
use std::sync::Arc;

use super::{Generation, Operation, Response, Shared};
use crate::review;

pub(super) struct Checkpoint {
    baseline: String,
    baseline_generation: u64,
    scope: review::Scope,
    inspected: Arc<Generation>,
    pending: Option<Pending>,
    acknowledged: Option<String>,
}

struct Pending {
    token: String,
    generation: Arc<Generation>,
    ready: bool,
}

pub(super) fn run(shared: &Shared, operation: Operation) -> Response {
    let mut state = shared.state.lock().expect("service state poisoned");
    let mut response = shared.response(&state);
    match operation {
        Operation::Review {
            name,
            baseline,
            baseline_generation,
            mut scope,
        } => {
            if name.is_empty() || name.len() > 256 {
                response.error = Some("review names must contain 1–256 bytes".to_owned());
                return response;
            }
            if let Err(error) = scope.normalize() {
                response.error = Some(error);
                return response;
            }
            let Some(current) = state.current.clone() else {
                response.error = Some("initial index is not ready".to_owned());
                return response;
            };
            if !state.reviews.contains_key(&name) {
                // Seed from an existing immutable checkpoint, NEVER the current publication.
                let Some(inspected) = state.checkpoints.get(&baseline).cloned() else {
                    response.error = Some(format!("unknown checkpoint: {baseline}"));
                    return response;
                };
                if inspected.number != baseline_generation {
                    response.error = Some("session checkpoint generation changed".to_owned());
                    return response;
                }
                // Reserve two pins per review, so repeated results cannot exhaust retention.
                if state.checkpoints.len() + 2 * state.reviews.len() + 2 > 64 {
                    response.error = Some(
                        "checkpoint limit reached; forget unused checkpoints/reviews".to_owned(),
                    );
                    return response;
                }
                state.reviews.insert(
                    name.clone(),
                    Checkpoint {
                        baseline: baseline.clone(),
                        baseline_generation,
                        scope: scope.clone(),
                        inspected,
                        pending: None,
                        acknowledged: None,
                    },
                );
                response.review_initialized = true;
            }
            response.review_name = Some(name.clone());
            let checkpoint = &state.reviews[&name];
            if checkpoint.scope != scope
                || checkpoint.baseline != baseline
                || checkpoint.baseline_generation != baseline_generation
            {
                response.error =
                    Some("review scope/baseline changed; use a separate review name".to_owned());
                return response;
            }
            let before = checkpoint.inspected.clone();
            state.next_review = state
                .next_review
                .checked_add(1)
                .expect("review token exhausted");
            let token = format!("{}:{}", shared.instance, state.next_review);
            // Supersede older results, including computations still in flight.
            state.reviews.get_mut(&name).unwrap().pending = Some(Pending {
                token: token.clone(),
                generation: current.clone(),
                ready: false,
            });
            response.checkpoint_generation = Some(before.number);
            drop(state);
            match before.snapshot.review(&current.snapshot, &scope) {
                Ok(output) => {
                    let mut state = shared.state.lock().expect("service state poisoned");
                    if let Some(pending) = state
                        .reviews
                        .get_mut(&name)
                        .and_then(|review| review.pending.as_mut())
                        && pending.token == token
                    {
                        pending.ready = true;
                    }
                    response.truncated = output.truncated;
                    response.review = Some(output);
                    response.review_token = Some(token);
                }
                Err(error) => response.error = Some(super::protocol::diagnostic(error)),
            }
        }
        Operation::Acknowledge { name, token } => {
            let Some(checkpoint) = state.reviews.get_mut(&name) else {
                response.error = Some(format!("unknown review: {name}"));
                return response;
            };
            if checkpoint.acknowledged.as_ref() == Some(&token) {
                response.checkpoint_generation = Some(checkpoint.inspected.number);
                return response; // Safe retry after a lost acknowledgement reply.
            }
            if !checkpoint
                .pending
                .as_ref()
                .is_some_and(|pending| pending.token == token && pending.ready)
            {
                response.error =
                    Some("stale or unknown review token; inspect a new review".to_owned());
                return response;
            }
            checkpoint.inspected = checkpoint.pending.take().unwrap().generation;
            checkpoint.acknowledged = Some(token);
            response.checkpoint_generation = Some(checkpoint.inspected.number);
        }
        Operation::ForgetReview { name } => {
            if state.reviews.remove(&name).is_none() {
                response.error = Some(format!("unknown review: {name}"));
            }
        }
        _ => unreachable!("only review operations"),
    }
    response
}
