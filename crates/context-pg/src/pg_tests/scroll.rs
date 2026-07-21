#[pg_test]
fn scroll_returns_stable_point_id_pages() {
    create_search_collection("m5_scroll_docs");
    upsert_search_points("m5_scroll_docs", &["10", "20", "30"]);

    let first_page = scroll_rows(
        "SELECT point_id, source_key, next_cursor
           FROM pgcontext.scroll('m5_scroll_docs', NULL, 2)",
    );

    assert_eq!(first_page.len(), 2);
    assert!(first_page[0].0 < first_page[1].0);
    assert_eq!(first_page[0].1, "10");
    assert_eq!(first_page[1].1, "20");

    let second_page = scroll_rows(&format!(
        "SELECT point_id, source_key, next_cursor
           FROM pgcontext.scroll('m5_scroll_docs', '{}', 2)",
        first_page[1].2
    ));

    assert_eq!(second_page.len(), 1);
    assert!(first_page[1].0 < second_page[0].0);
    assert_eq!(second_page[0].1, "30");
}

#[pg_test]
#[should_panic(expected = "invalid scroll cursor checksum")]
fn scroll_rejects_tampered_cursors() {
    create_search_collection("m5_scroll_tampered");
    upsert_search_points("m5_scroll_tampered", &["10"]);

    let first_page = scroll_rows(
        "SELECT point_id, source_key, next_cursor
           FROM pgcontext.scroll('m5_scroll_tampered', NULL, 1)",
    );
    let tampered = tamper_cursor_point_id(&first_page[0].2);

    Spi::run(&format!(
        "SELECT pgcontext.scroll('m5_scroll_tampered', '{tampered}', 1)"
    ))
    .expect("tampered cursor should be rejected");
}

#[pg_test]
#[should_panic(expected = "scroll cursor belongs to collection")]
fn scroll_rejects_stale_collection_cursors() {
    create_search_collection("m5_scroll_source");
    upsert_search_points("m5_scroll_source", &["10"]);
    create_search_collection("m5_scroll_target");
    upsert_search_points("m5_scroll_target", &["10"]);

    let first_page = scroll_rows(
        "SELECT point_id, source_key, next_cursor
           FROM pgcontext.scroll('m5_scroll_source', NULL, 1)",
    );

    Spi::run(&format!(
        "SELECT pgcontext.scroll('m5_scroll_target', '{}', 1)",
        first_page[0].2
    ))
    .expect("stale collection cursor should be rejected");
}

fn scroll_rows(sql: &str) -> Vec<(i64, String, String)> {
    Spi::connect(|client| {
        let rows = client.select(sql, None, &[])?;
        let mut output = Vec::new();
        for row in rows {
            output.push((
                row.get::<i64>(1)?.expect("point_id should not be null"),
                row.get::<String>(2)?.expect("source_key should not be null"),
                row.get::<String>(3)?
                    .expect("next_cursor should not be null"),
            ));
        }
        Ok::<_, spi::Error>(output)
    })
    .expect("scroll query failed")
}

fn tamper_cursor_point_id(cursor: &str) -> String {
    let mut parts = cursor.split(':').collect::<Vec<_>>();
    assert_eq!(parts.len(), 4);
    parts[2] = if parts[2] == "1" { "2" } else { "1" };
    parts.join(":")
}
