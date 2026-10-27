//! JSON filesystem methods answer from bounded workers without blocking runtime state.
use super::responses::{encode_error, encode_success};
use crate::api::schema::{Method, Request};
use crate::app::App;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    mpsc::Sender,
    Arc, Mutex,
};

#[derive(Default)]
pub(crate) struct FileService {
    searches: Mutex<HashMap<String, Arc<AtomicU64>>>,
    mutations: Mutex<()>,
}
impl FileService {
    pub(crate) fn cancel(&self, params: crate::api::schema::FilesCancelParams) {
        if let Ok(mut searches) = self.searches.lock() {
            if searches.len() < 128 || searches.contains_key(&params.search_id) {
                let latest = searches
                    .entry(params.search_id)
                    .or_insert_with(|| Arc::new(AtomicU64::new(params.generation)));
                latest.fetch_max(params.generation, Ordering::Relaxed);
            }
        }
    }
}
impl App {
    pub(crate) fn handle_deferred_files_api_request(
        &mut self,
        request: Request,
        respond_to: Sender<String>,
    ) {
        let service = self.file_service.clone();
        if let Method::FilesCancel(params) = request.method {
            service.cancel(params);
            let _ = respond_to.send(encode_success(
                request.id,
                crate::api::schema::ResponseResult::Ok {},
            ));
            return;
        }

        let latest = if let Method::FilesSearch(params) = &request.method {
            let Ok(mut searches) = service.searches.lock() else {
                let _ = respond_to.send(encode_error(
                    request.id,
                    "file_error",
                    "search state unavailable",
                ));
                return;
            };
            if searches.len() >= 128 && !searches.contains_key(&params.search_id) {
                searches.retain(|_, generation| Arc::strong_count(generation) > 1);
            }
            if searches.len() >= 128 && !searches.contains_key(&params.search_id) {
                let _ = respond_to.send(encode_error(request.id, "file_busy", "too many searches"));
                return;
            }
            let latest = searches
                .entry(params.search_id.clone())
                .or_insert_with(|| Arc::new(AtomicU64::new(params.generation)))
                .clone();
            if latest.load(Ordering::Relaxed) > params.generation {
                let _ = respond_to.send(encode_success(
                    request.id,
                    crate::api::schema::ResponseResult::FileSearch {
                        matches: Vec::new(),
                        truncated: false,
                        cancelled: true,
                        warnings: Vec::new(),
                    },
                ));
                return;
            }
            latest.store(params.generation, Ordering::Relaxed);
            Some(latest)
        } else {
            None
        };
        let Ok(permit) = self.directory_read_slots.clone().try_acquire_owned() else {
            let _ = respond_to.send(encode_error(
                request.id,
                "file_busy",
                "too many filesystem requests; retry shortly",
            ));
            return;
        };
        let data = (request, respond_to, service, latest, permit);
        if let Err(((request, respond_to, ..), error)) = crate::thread_spawn::spawn_named_with(
            "herdr-files",
            data,
            |(request, respond_to, service, latest, _permit)| {
                let result = match request.method {
                    Method::FilesPlan(params) => crate::files::plan(params),
                    Method::FileRead(params) => crate::files::read_document(params),
                    Method::FileStat(params) => crate::files::stat(params),
                    Method::FilesSearch(params) => match latest {
                        Some(latest) => crate::files::search(params, &latest),
                        None => Err(crate::files::failure("missing search generation")),
                    },
                    Method::FilesReplace(params) => match service.mutations.lock() {
                        Ok(_guard) => crate::files::replace(params),
                        Err(_) => Err(crate::files::failure("file mutation state unavailable")),
                    },
                    Method::FilesOperate(params) => match service.mutations.lock() {
                        Ok(_guard) => crate::files::operate(params),
                        Err(_) => Err(crate::files::failure("file mutation state unavailable")),
                    },
                    _ => Err(crate::files::failure("not a filesystem method")),
                };
                let response = match result {
                    Ok(result) => encode_success(request.id, result),
                    Err((code, message)) => encode_error(request.id, code, message),
                };
                let _ = respond_to.send(response);
            },
        ) {
            let _ = respond_to.send(encode_error(
                request.id,
                "file_error",
                format!("cannot start filesystem worker: {error}"),
            ));
        }
    }
}
