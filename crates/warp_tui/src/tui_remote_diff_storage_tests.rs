use std::fs;
use std::os::unix::net::UnixStream;
use std::process::Stdio;
use std::sync::Mutex;

use async_io::Async;
use remote_server::auth::RemoteServerAuthContext;
use remote_server::client::{RemoteServerClient, spawn_stderr_forwarder};
use remote_server::manager::{
    RemoteServerExitStatus, RemoteServerManager, RemoteServerManagerEvent,
};
use remote_server::proto::{
    InitializeResponse, RenameAndSaveFileResponse, RenameAndSaveFileSuccess, ServerMessage,
    client_message, host_scoped_request, rename_and_save_file_response, server_message,
};
use remote_server::protocol::{read_client_message, write_server_message};
use remote_server::setup::{PreinstallCheckResult, RemotePlatform};
use remote_server::transport::{Connection, ControlPath, Error, InstallOutcome, RemoteTransport};
use warp_core::SessionId;
use warp_core::channel::ChannelState;
use warpui::App;
use warpui::r#async::executor;

use super::*;

#[derive(Debug)]
struct SocketTransport {
    socket: Mutex<Option<UnixStream>>,
}

impl RemoteTransport for SocketTransport {
    fn detect_platform(&self) -> BoxFuture<'static, Result<RemotePlatform, Error>> {
        unreachable!()
    }

    fn run_preinstall_check(&self) -> BoxFuture<'static, Result<PreinstallCheckResult, Error>> {
        unreachable!()
    }

    fn check_binary(&self) -> BoxFuture<'static, Result<bool, Error>> {
        unreachable!()
    }

    fn check_has_old_binary(&self) -> BoxFuture<'static, anyhow::Result<bool>> {
        unreachable!()
    }

    fn install_binary(&self) -> BoxFuture<'static, InstallOutcome> {
        unreachable!()
    }

    fn remove_remote_server_binary(&self) -> BoxFuture<'static, anyhow::Result<()>> {
        unreachable!()
    }

    fn is_reconnectable(&self, _: Option<&RemoteServerExitStatus>) -> bool {
        false
    }

    fn connect(
        &self,
        executor: Arc<executor::Background>,
    ) -> BoxFuture<'static, anyhow::Result<Connection>> {
        let socket = self.socket.lock().unwrap().take().unwrap();
        async move {
            let reader = Async::new(socket.try_clone()?)?;
            let writer = Async::new(socket)?;
            let (client, event_rx, failure_rx, host_response_rx) =
                RemoteServerClient::new(reader, writer, &executor);
            let stderr_tail = spawn_stderr_forwarder(futures::io::empty(), &executor);
            let child = command::r#async::Command::new("cat")
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .kill_on_drop(true)
                .spawn()?;
            Ok(Connection {
                client,
                event_rx,
                failure_rx,
                host_response_rx,
                child,
                control_path: ControlPath::None,
                stderr_tail,
            })
        }
        .boxed()
    }
}

#[test]
fn accept_remote_move_sends_native_move_request_and_reports_destination() {
    App::test((), |mut app| async move {
        app.add_singleton_model(FileModel::new);
        let manager = app.add_singleton_model(RemoteServerManager::new);
        let (client_socket, server_socket) = UnixStream::pair().unwrap();
        manager.update(&mut app, |_, ctx| {
            let mut reader = Async::new(server_socket.try_clone().unwrap()).unwrap();
            let mut writer = Async::new(server_socket).unwrap();
            ctx.background_executor()
                .spawn(async move {
                    let initialize = read_client_message(&mut reader).await.unwrap();
                    write_server_message(
                        &mut writer,
                        &ServerMessage {
                            request_id: initialize.request_id,
                            message: Some(server_message::Message::InitializeResponse(
                                InitializeResponse {
                                    server_version: ChannelState::app_version()
                                        .unwrap_or_default()
                                        .to_owned(),
                                    host_id: "remote-file-edits".to_owned(),
                                },
                            )),
                        },
                    )
                    .await
                    .unwrap();
                    let request = read_client_message(&mut reader).await.unwrap();
                    let Some(client_message::Message::HostScoped(request_body)) = request.message
                    else {
                        panic!("expected host-scoped move request");
                    };
                    let Some(host_scoped_request::Message::RenameAndSaveFile(move_request)) =
                        request_body.message
                    else {
                        panic!(
                            "expected native rename-and-save instead of separate write and delete"
                        );
                    };
                    fs::write(&move_request.path, move_request.content).unwrap();
                    fs::rename(&move_request.path, &move_request.new_path).unwrap();
                    write_server_message(
                        &mut writer,
                        &ServerMessage {
                            request_id: request.request_id,
                            message: Some(server_message::Message::RenameAndSaveFileResponse(
                                RenameAndSaveFileResponse {
                                    result: Some(rename_and_save_file_response::Result::Success(
                                        RenameAndSaveFileSuccess {},
                                    )),
                                },
                            )),
                        },
                    )
                    .await
                    .unwrap();
                    let _ = read_client_message(&mut reader).await;
                })
                .detach();
        });
        let session_id = SessionId::default();
        let (connected_tx, connected_rx) = async_channel::bounded(1);
        app.update(|ctx| {
            ctx.subscribe_to_model(&manager, move |_, event, _| match event {
                RemoteServerManagerEvent::SessionConnected { host_id, .. } => {
                    connected_tx.try_send(Ok(host_id.clone())).unwrap();
                }
                RemoteServerManagerEvent::SessionConnectionFailed { error, .. } => {
                    connected_tx.try_send(Err(error.clone())).unwrap();
                }
                _ => {}
            })
        });
        manager.update(&mut app, |manager, ctx| {
            manager.connect_session(
                session_id,
                SocketTransport {
                    socket: Mutex::new(Some(client_socket)),
                },
                Arc::new(RemoteServerAuthContext::new(
                    || futures::future::ready(None).boxed(),
                    || "file-edits-test".to_owned(),
                    "user".to_owned(),
                    "user@example.com".to_owned(),
                    false,
                )),
                None,
                ctx,
            )
        });
        let host_id = connected_rx.recv().await.unwrap().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source.txt");
        let destination = dir.path().join("destination.txt");
        fs::write(&source, "original\n").unwrap();
        let storage = app.add_model(|_| {
            TuiDiffStorage::new(
                vec![FileDiff::new(
                    "original\n".to_owned(),
                    source.to_str().unwrap().to_owned(),
                    DiffType::update(
                        vec![DiffDelta {
                            replacement_line_range: 1..2,
                            insertion: "updated\n".to_owned(),
                        }],
                        Some(destination.to_str().unwrap().to_owned()),
                    ),
                )],
                DiffSessionType::Remote(host_id),
            )
        });

        let result = storage
            .update(&mut app, |storage, ctx| storage.accept_and_save(ctx))
            .await;

        let RequestFileEditsResult::Success {
            deleted_files,
            updated_files,
            ..
        } = result
        else {
            panic!("expected remote move success");
        };
        assert_eq!(deleted_files, vec![source.to_str().unwrap().to_owned()]);
        assert_eq!(
            updated_files[0].file_context.file_name,
            destination.to_str().unwrap()
        );
        assert!(!source.exists());
        assert_eq!(fs::read_to_string(&destination).unwrap(), "updated\n");
    });
}
