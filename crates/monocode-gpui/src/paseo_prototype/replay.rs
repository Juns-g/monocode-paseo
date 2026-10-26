//! Read-only replay of parent-redacted snapshots through the production view.
use super::*;

struct Replay {
    prototype: Entity<Prototype>,
    measure: bool,
}

#[gpui::test]
fn history_replacement_keeps_rendered_message_owners_valid(cx: &mut gpui::TestAppContext) {
    replacement_case(cx, true);
}
#[gpui::test]
fn original_history_replacement_keeps_rendered_message_owners_valid(cx: &mut gpui::TestAppContext) {
    replacement_case(cx, false);
}
fn replacement_case(cx: &mut gpui::TestAppContext, preserve_prose: bool) {
    cx.update(super::tests::configure_test);
    cx.update(|cx| cx.set_reduce_motion(false));
    let window = cx.open_window(gpui::size(px(1180.), px(800.)), |_, cx| Replay {
        prototype: cx.new(Prototype::new),
        measure: false,
    });
    for (step, entries) in [
            json!([
                {"item":{"type":"user_message","text":"Synthetic question"}},
                {"item":{"type":"assistant_message","text":"Synthetic answer"}}
            ]),
            json!([{ "item":{"type":"assistant_message","text":"Long synthetic continuation. ".repeat(200)}}]),
        ].into_iter().enumerate() {
            eprintln!("replacement preserve_prose={preserve_prose} step={step}");
            window.update(cx, |replay, _, cx| {
                replay.prototype.update(cx, |view, cx| {
                    view.server="s".into(); view.status="connected".into();
                    view.open("a".into(),cx);
                    if step == 0 && !preserve_prose {
                        view.transcript=cx.new(|_|TranscriptView::new());
                        view.transcripts.insert("a".into(),view.transcript.clone());
                    }
                    view.receive(json!({"event":"snapshot","serverId":"s","agentId":"a","agent":{"id":"a","status":"idle"},"entries":entries}),cx);
                });
                replay.measure=true;cx.notify();
            }).unwrap();
            cx.refresh().unwrap();cx.run_until_parked();
        }
}
impl Render for Replay {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let measure = std::mem::take(&mut self.measure);
        let prototype = self.prototype.clone();
        div()
            .size_full()
            .relative()
            .child(self.prototype.clone())
            .child(
                gpui::canvas(
                    move |_, window, cx| {
                        if measure {
                            prototype.update(cx, |view, cx| {
                                view.transcript.update(cx, |transcript, cx| {
                                    transcript.test_measure_all_rows(window, cx);
                                });
                            });
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}

#[gpui::test]
#[ignore = "Requires parent-redacted snapshots; set PASEO_R7_REPLAY_DIR (no Metal or daemon)"]
fn replay_real_shapes_with_native_measurement(cx: &mut gpui::TestAppContext) {
    let root =
        PathBuf::from(std::env::var_os("PASEO_R7_REPLAY_DIR").expect("Set PASEO_R7_REPLAY_DIR"));
    let mut paths = std::fs::read_dir(root)
        .unwrap()
        .map(|p| p.unwrap().path())
        .filter(|p| p.extension().is_some_and(|ext| ext == "json"))
        .collect::<Vec<_>>();
    paths.sort();
    assert_eq!(paths.len(), 16);
    let traces = paths
        .iter()
        .map(|path| serde_json::from_slice::<Vec<Value>>(&std::fs::read(path).unwrap()).unwrap())
        .collect::<Vec<_>>();
    cx.update(super::tests::configure_test);
    cx.update(|cx| cx.set_reduce_motion(false));
    let window = cx.open_window(gpui::size(px(1180.), px(800.)), |_, cx| Replay {
        prototype: cx.new(Prototype::new),
        measure: false,
    });
    let step = |file: usize, event: usize, cx: &mut gpui::TestAppContext| {
        eprintln!("replay file={file} event={event}");
        let snapshot = traces[file][event].clone();
        window
            .update(cx, |replay, _, cx| {
                replay.prototype.update(cx, |view, cx| {
                    view.server = snapshot["serverId"].as_str().unwrap().into();
                    view.status = "connected".into();
                    view.open(snapshot["agentId"].as_str().unwrap().into(), cx);
                    view.receive(snapshot, cx);
                });
                replay.measure = true;
                cx.notify();
            })
            .unwrap();
        cx.refresh().unwrap();
        cx.run_until_parked();
    };
    for (file, trace) in traces.iter().enumerate() {
        for event in 0..trace.len() {
            step(file, event, cx);
        }
    }
    for event in 0..traces.iter().map(Vec::len).max().unwrap() {
        for (file, trace) in traces.iter().enumerate() {
            if event < trace.len() {
                step(file, event, cx);
            }
        }
    }
}
