use warpui_core::App;

use super::{NETWORK_LOGGING_MAX_ITEMS, NetworkLogItem, NetworkLogModel};

#[test]
fn external_transfers_keep_their_transport_and_redacted_log_hooks() {
    App::test((), |mut app| async move {
        let mut server = mockito::Server::new();
        let api_request = server
            .mock("GET", "/api")
            .match_header("authorization", "Bearer test-default-auth")
            .match_header("cookie", "session=test-default-cookie")
            .match_header("proxy-authorization", "Bearer test-default-iap")
            .match_header("x-warp-os-category", mockito::Matcher::Any)
            .with_status(200)
            .create();
        let query = "X-Amz-Signature=test%2Bsignature&key=a%2Fb&key=c+%20d";
        let mut storage_request = server
            .mock("PUT", "/signed-object")
            .match_query(mockito::Matcher::Exact(query.into()))
            .match_header("authorization", "AWS4-HMAC-SHA256 test-signature")
            .match_header("x-amz-checksum-crc32c", "EjRWeA==")
            .match_body(r#"{"credential":"test-request-body-secret"}"#)
            .with_header("set-cookie", "test-response-cookie-secret")
            .with_status(200);
        for header in [
            "cookie",
            "proxy-authorization",
            "x-warp-os-category",
            "x-warp-client-id",
            "x-warp-client-version",
            "x-oz-api-source",
        ] {
            storage_request = storage_request.match_header(header, mockito::Matcher::Missing);
        }
        let storage_request = storage_request.create();
        let mut defaults = http::HeaderMap::new();
        for (name, value) in [
            ("authorization", "Bearer test-default-auth"),
            ("cookie", "session=test-default-cookie"),
            ("proxy-authorization", "Bearer test-default-iap"),
        ] {
            defaults.insert(
                http::header::HeaderName::from_bytes(name.as_bytes()).unwrap(),
                http::HeaderValue::from_static(value),
            );
        }
        let mut api_client = http_client::Client::from_client_builder(
            reqwest::Client::builder().default_headers(defaults),
        )
        .unwrap();
        let mut external_client = http_client::Client::new_external();
        let model = app.add_singleton_model(|_| NetworkLogModel::default());
        let (logged, wait_for_log) = futures::channel::oneshot::channel();
        let mut logged = Some(logged);
        app.update(|ctx| {
            ctx.observe_model(&model, move |model, ctx| {
                if model.as_ref(ctx).len() == 4
                    && let Some(logged) = logged.take()
                {
                    let _ = logged.send(model.as_ref(ctx).snapshot_text());
                }
            })
        });
        model.update(&mut app, |model, ctx| {
            model.install_on_clients([&mut api_client, &mut external_client], ctx);
        });
        api_client
            .get(format!("{}/api", server.url()))
            .send()
            .await
            .unwrap();
        let request = external_client
            .external_request(
                http::Method::PUT,
                format!("{}/signed-object?{query}", server.url()),
            )
            .header("authorization", "AWS4-HMAC-SHA256 test-signature")
            .header("x-amz-checksum-crc32c", "EjRWeA==")
            .json(&serde_json::json!({"credential": "test-request-body-secret"}))
            .build()
            .unwrap();
        let response = api_client.execute(request).await.unwrap();
        assert_eq!(response.status(), http::StatusCode::OK);
        let snapshot = match futures::future::select(
            wait_for_log,
            Box::pin(warpui_core::r#async::Timer::after(
                std::time::Duration::from_secs(5),
            )),
        )
        .await
        {
            futures::future::Either::Left((snapshot, _)) => snapshot.unwrap(),
            futures::future::Either::Right(_) => panic!("network hooks did not populate the log"),
        };
        assert!(snapshot.contains("x-warp-os-category"));
        assert!(snapshot.contains(&format!("External request PUT {}", server.url())));
        assert!(snapshot.contains(&format!("External response 200 OK {}", server.url())));
        for secret in [
            "signed-object",
            "X-Amz-Signature",
            "test%2Bsignature",
            "test-signature",
            "EjRWeA==",
            "test-request-body-secret",
            "test-response-cookie-secret",
        ] {
            assert!(!snapshot.contains(secret));
        }
        api_request.assert();
        storage_request.assert();
    });
}

#[test]
fn empty_snapshot_is_empty_string() {
    App::test((), |app| async move {
        let model = app.add_singleton_model(|_| NetworkLogModel::default());
        model.read(&app, |model, _| {
            assert_eq!(model.snapshot_text(), "");
            assert_eq!(model.len(), 0);
        });
    });
}

#[test]
fn snapshot_joins_items_with_newlines() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| NetworkLogModel::default());
        model.update(&mut app, |model, ctx| {
            model.push(NetworkLogItem::from_string("first"), ctx);
            model.push(NetworkLogItem::from_string("second"), ctx);
            model.push(NetworkLogItem::from_string("third"), ctx);
        });
        model.read(&app, |model, _| {
            assert_eq!(model.snapshot_text(), "first\nsecond\nthird");
            assert_eq!(model.len(), 3);
        });
    });
}

#[test]
fn push_beyond_capacity_drops_oldest() {
    App::test((), |mut app| async move {
        let model = app.add_singleton_model(|_| NetworkLogModel::default());
        // Push exactly the capacity; the snapshot should contain all items
        // and the count should equal the capacity.
        model.update(&mut app, |model, ctx| {
            for i in 0..NETWORK_LOGGING_MAX_ITEMS {
                model.push(NetworkLogItem::from_string(format!("item-{i}")), ctx);
            }
        });
        model.read(&app, |model, _| {
            assert_eq!(model.len(), NETWORK_LOGGING_MAX_ITEMS);
            // The oldest item is still present when we are exactly at capacity.
            assert!(model.snapshot_text().starts_with("item-0\n"));
        });

        // Push one more: the oldest item should be evicted so the store stays
        // at capacity, and the snapshot should start at item-1 now.
        model.update(&mut app, |model, ctx| {
            model.push(NetworkLogItem::from_string("overflow"), ctx);
        });
        model.read(&app, |model, _| {
            assert_eq!(model.len(), NETWORK_LOGGING_MAX_ITEMS);
            assert!(!model.snapshot_text().contains("item-0\n"));
            assert!(model.snapshot_text().starts_with("item-1\n"));
            assert!(model.snapshot_text().ends_with("\noverflow"));
        });

        // Pushing many additional items keeps the store at capacity.
        model.update(&mut app, |model, ctx| {
            for i in 0..10 {
                model.push(NetworkLogItem::from_string(format!("extra-{i}")), ctx);
            }
        });
        model.read(&app, |model, _| {
            assert_eq!(model.len(), NETWORK_LOGGING_MAX_ITEMS);
        });
    });
}
