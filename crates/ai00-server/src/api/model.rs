use ai00_core::{
    reload::State,
    run::{InitState, StateId},
    ReloadRequest, RuntimeInfo, SaveRequest, ThreadRequest,
};
use futures_util::StreamExt;
use salvo::{oapi::extract::JsonBody, prelude::*};
use serde::Serialize;
use web_rwkv::runtime::model::ModelInfo;

use super::*;
use crate::{build_path, types::ThreadState, SLEEP};

#[derive(Debug, Clone, Serialize)]
pub struct InfoResponse {
    reload: ReloadRequest,
    model: ModelInfo,
    states: Vec<InitStateInfo>,
}

#[derive(Debug, Clone, Serialize)]
struct InitStateInfo {
    id: StateId,
    name: String,
}

/// Report the current runtime info.
#[handler]
pub async fn info(depot: &mut Depot) -> Json<InfoResponse> {
    let ThreadState { sender, .. } = depot.obtain::<ThreadState>().unwrap();
    let RuntimeInfo {
        reload,
        model,
        states,
        ..
    } = request_info(sender.to_owned(), SLEEP).await;
    let states = states
        .into_iter()
        .map(|(id, InitState { name, .. })| InitStateInfo { id, name })
        .collect();
    Json(InfoResponse {
        reload,
        model,
        states,
    })
}

/// Report the current runtime info every half second.
///
/// `/api/models/state`.
#[handler]
pub async fn state(depot: &mut Depot, res: &mut Response) {
    let ThreadState { sender, .. } = depot.obtain::<ThreadState>().unwrap();
    let (info_sender, info_receiver) = flume::unbounded();
    let task = request_info_stream(sender.to_owned(), info_sender, SLEEP);
    tokio::task::spawn(task);

    let stream = info_receiver.into_stream().map(
        |RuntimeInfo {
             reload,
             model,
             states,
             ..
         }| {
            let states = states
                .into_iter()
                .map(|(id, InitState { name, .. })| InitStateInfo { id, name })
                .collect();
            match serde_json::to_string(&InfoResponse {
                reload,
                model,
                states,
            }) {
                Ok(json) => SseEvent::default().json(json),
                Err(err) => Err(err),
            }
        },
    );

    salvo::sse::stream(res, stream);
}

/// Load a runtime with models, LoRA, initial states, etc.
///
/// `/api/models/load`.
#[handler]
pub async fn load(depot: &mut Depot, req: JsonBody<ReloadRequest>) -> StatusCode {
    let ThreadState { sender, path } = depot.obtain::<ThreadState>().unwrap();
    let (result_sender, result_receiver) = flume::unbounded();
    let mut request = req.0;

    // make sure that we are not visiting un-permitted path.
    request.model_path = match build_path(path, request.model_path) {
        Ok(path) => path,
        Err(_) => return StatusCode::NOT_FOUND,
    };
    for x in request.lora.iter_mut() {
        x.path = match build_path(path, &x.path) {
            Ok(path) => path,
            Err(_) => return StatusCode::NOT_FOUND,
        }
    }
    for x in request.state.iter_mut() {
        x.path = match build_path(path, &x.path) {
            Ok(path) => path,
            Err(_) => return StatusCode::NOT_FOUND,
        }
    }

    let _ = sender.send(ThreadRequest::Reload {
        request: Box::new(request),
        sender: Some(result_sender),
    });
    match result_receiver.recv_async().await.unwrap() {
        true => StatusCode::OK,
        false => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Unload the current runtime.
///
/// `/api/models/unload`.
#[handler]
pub async fn unload(depot: &mut Depot) -> StatusCode {
    let ThreadState { sender, .. } = depot.obtain::<ThreadState>().unwrap();
    let _ = sender.send(ThreadRequest::Unload);
    while try_request_info(sender.clone()).await.is_ok() {}
    StatusCode::OK
}

/// Load an initial state from the path.
///
/// `/api/models/state/load`.
#[handler]
pub async fn load_state(depot: &mut Depot, req: JsonBody<State>) -> StatusCode {
    let ThreadState { sender, path } = depot.obtain::<ThreadState>().unwrap();
    let (result_sender, result_receiver) = flume::unbounded();
    let mut request = req.0;

    request.path = match build_path(path, &request.path) {
        Ok(path) => path,
        Err(_) => return StatusCode::NOT_FOUND,
    };

    let _ = sender.send(ThreadRequest::StateLoad {
        request,
        sender: Some(result_sender),
    });
    match result_receiver.recv_async().await.unwrap() {
        true => StatusCode::OK,
        false => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Save the current model as a prefab.
///
/// `/api/models/save`.
#[handler]
pub async fn save(depot: &mut Depot, req: JsonBody<SaveRequest>) -> StatusCode {
    let ThreadState { sender, path } = depot.obtain::<ThreadState>().unwrap();
    let (result_sender, result_receiver) = flume::unbounded();
    let mut request = req.0;

    // make sure that we are not visiting un-permitted path.
    request.path = match build_path(path, request.path) {
        Ok(path) => path,
        Err(_) => return StatusCode::NOT_FOUND,
    };

    let _ = sender.send(ThreadRequest::Save {
        request,
        sender: result_sender,
    });
    match result_receiver.recv_async().await.unwrap() {
        true => StatusCode::OK,
        false => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
