mod common;

use herdr_file_viewer::config::ImageProtocol;
use herdr_file_viewer::controller::{
    Components, ContentProvider, Controller, EditorHandoff, EditorOutcome, GitService,
    RenderResult, RootProviders, clear_kitty_images,
};
use herdr_file_viewer::git::{Baseline, Status};
use herdr_file_viewer::image_preview::Paint;
use herdr_file_viewer::intent::Intent;
use herdr_file_viewer::presenter::{self, FinderView, Focus, ViewState};
use herdr_file_viewer::view_policy::{
    FileDescriptor, ViewMode, applicable_modes, default_mode, is_image,
};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::text::Text;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[test]
fn is_image_recognizes_supported_extensions_case_insensitively() {
    let images = [
        "photo.png",
        "photo.PNG",
        "pic.jpg",
        "pic.JPG",
        "image.jpeg",
        "anim.gif",
        "modern.webp",
        "bitmap.bmp",
        "favicon.ico",
        "scan.tiff",
        "scan.tif",
    ];
    for name in images {
        assert!(
            is_image(Path::new(name)),
            "{name} should be recognized as an image"
        );
    }

    let non_images = [
        "code.rs",
        "doc.md",
        "data.json",
        "archive.tar.gz",
        "binary.bin",
    ];
    for name in non_images {
        assert!(
            !is_image(Path::new(name)),
            "{name} should not be recognized as an image"
        );
    }
}

#[test]
fn default_mode_for_image_is_image_view() {
    let desc = FileDescriptor {
        path: PathBuf::from("picture.png"),
        is_markdown: false,
        is_changed: false,
        is_image: true,
    };
    assert_eq!(default_mode(&desc), ViewMode::ImageView);
    assert_eq!(
        applicable_modes(&desc),
        vec![ViewMode::ImageView, ViewMode::SyntaxContent]
    );

    // A git-changed image still previews; `v` can cycle to the binary diff.
    let changed_desc = FileDescriptor {
        path: PathBuf::from("picture.png"),
        is_markdown: false,
        is_changed: true,
        is_image: true,
    };
    assert_eq!(default_mode(&changed_desc), ViewMode::ImageView);
    assert!(applicable_modes(&changed_desc).contains(&ViewMode::Diff));
}

#[test]
fn presenter_draws_image_without_panicking() {
    let img = image::DynamicImage::new_rgb8(10, 10);
    let proto = Paint::from_kitty_png(&img, 40, 20, false, 10, 20).expect("png encode");
    draw_ok(&image_view_state(proto));
}

#[test]
fn presenter_skips_graphics_when_a_modal_overlay_is_open() {
    let img = image::DynamicImage::new_rgb8(10, 10);
    let proto = Paint::from_kitty_png(&img, 40, 20, false, 10, 20).expect("png encode");
    let mut state = image_view_state(proto);
    state.finder = Some(FinderView {
        query: String::new(),
        scope_label: "workspace".into(),
        workspace: true,
        matches: Vec::new(),
        cursor: 0,
        hscroll: 0,
    });
    draw_ok(&state);
}

fn image_view_state(proto: Paint) -> ViewState {
    ViewState {
        nodes: Vec::new(),
        selected: 0,
        content: Text::raw("10 × 10 px • 300 B"),
        notices: Vec::new(),
        flash: None,
        focus: Focus::Content,
        width: 80,
        content_scroll: 0,
        content_hscroll: 0,
        tree_scroll: 0,
        tree_follow_selection: true,
        tree_hscroll: 0,
        content_rows: 1,
        wrap: false,
        content_pad_left: false,
        split_pct: 30,
        tree_position: herdr_file_viewer::config::TreePosition::Left,
        tree_max_cols: 40,
        tree_icons: herdr_file_viewer::config::TreeIcons::Off,
        split_manual: false,
        zoomed: false,
        pinned: false,
        update_banner: None,
        picker: None,
        finder: None,
        workspace_search: None,
        annotation_count: 0,
        annotation_overview: None,
        annotation_editor: None,
        discard_confirm: None,
        annotation_indicators: presenter::AnnotationIndicatorsView::default(),
        root_name: "test".to_string(),
        branch: None,
        prompt: None,
        content_title: Some("test.png".to_string()),
        content_rendering: false,
        search: None,
        line_select: None,
        content_selection: None,
        help: None,
        context_menu: None,
        image: Some(Arc::new(Mutex::new(proto))),
    }
}

fn draw_ok(state: &ViewState) {
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| {
            let (cw, ch) = presenter::draw(f, state);
            assert!(cw > 0);
            assert!(ch > 0);
        })
        .unwrap();
}

#[test]
fn clear_kitty_images_runs_safely() {
    // Calling clear_kitty_images shouldn't panic or error
    clear_kitty_images();
}

/// Image → image must keep the live placement. Dropping it before the next
/// photo is transmitted is what made herdr retain every previous host image.
#[test]
fn switching_images_keeps_the_current_placement_until_the_next_one_replaces_it() {
    let dir = common::TempDir::new();
    std::fs::write(dir.path().join("a.png"), b"a").unwrap();
    std::fs::write(dir.path().join("b.png"), b"b").unwrap();
    std::fs::write(dir.path().join("c.txt"), b"c").unwrap();

    let mut ctrl = Controller::new(
        common::resolved(dir.path().to_path_buf(), false),
        Baseline::Head,
        Components {
            providers: Box::new(|_| RootProviders {
                git: Arc::new(NoGit),
                content: Box::new(ImageOrText),
            }),
            editor: Box::new(NoEditor),
            clipboard: Box::new(common::RecordingClipboard::default()),
            renderers: None,
        },
    );
    let picker =
        herdr_file_viewer::image_preview::init_picker(ImageProtocol::Kitty).expect("kitty picker");
    ctrl.set_image_picker(picker);
    ctrl.set_content_viewport(60, 20);

    wait_for(&mut ctrl, "img:a.png");
    assert!(
        ctrl.view_state().image.is_some(),
        "first image should be placed"
    );
    let placed = ctrl.view_state().image.clone().unwrap();

    ctrl.handle(Intent::NavDown);
    let kept = ctrl
        .view_state()
        .image
        .clone()
        .expect("moving to the next image must keep the current placement");
    assert!(
        Arc::ptr_eq(&placed, &kept),
        "the in-flight switch must not drop the placement"
    );
    let body = content_text(&ctrl);
    assert!(
        !body.contains("Rendering"),
        "the text placeholder would stop drawing the image, got {body}"
    );

    wait_for(&mut ctrl, "img:b.png");
    assert!(
        ctrl.view_state().image.is_some(),
        "the replacement preview should stay placed"
    );

    ctrl.handle(Intent::NavDown);
    assert!(
        ctrl.view_state().image.is_none(),
        "leaving an image for a non-image still clears the placement"
    );
}

fn content_text(ctrl: &Controller) -> String {
    ctrl.view_state()
        .content
        .lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn wait_for(ctrl: &mut Controller, marker: &str) {
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(2) {
        ctrl.poll();
        if content_text(ctrl).contains(marker) && ctrl.view_state().image.is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!(
        "timed out waiting for {marker}; body={}",
        content_text(ctrl)
    );
}

struct ImageOrText;
impl ContentProvider for ImageOrText {
    fn render(&self, path: &Path, mode: ViewMode, _: Option<&str>) -> RenderResult {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        if mode == ViewMode::ImageView {
            return RenderResult {
                content: Text::raw(format!("img:{name}")),
                notices: Vec::new(),
                source: None,
                image: Some(Arc::new(image::DynamicImage::new_rgb8(8, 8))),
            };
        }
        RenderResult {
            content: Text::raw(format!("text:{name}")),
            notices: Vec::new(),
            source: None,
            image: None,
        }
    }
}

struct NoGit;
impl GitService for NoGit {
    fn status(&self) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn changed_set(&self, _: Baseline) -> BTreeMap<PathBuf, Status> {
        BTreeMap::new()
    }
    fn diff(&self, _: &Path, _: Baseline, _: bool) -> String {
        String::new()
    }
    fn diff_directory(&self, _: &Path, _: Baseline) -> String {
        String::new()
    }
}

struct NoEditor;
impl EditorHandoff for NoEditor {
    fn open(&mut self, _: &Path) -> EditorOutcome {
        EditorOutcome::NoTakeover
    }
}
