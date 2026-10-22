use super::*;

#[test]
fn each_stage_runs_its_own_entry_point() {
    assert_eq!(ActionStage::Pre.entry_point(), "pre");
    assert_eq!(ActionStage::Main.entry_point(), "main");
    assert_eq!(ActionStage::Post.entry_point(), "post");
}

#[test]
fn pre_and_post_stages_prefix_the_main_display_name() {
    assert_eq!(ActionStage::Pre.display_name("Run cache"), "Pre Run cache");
    assert_eq!(ActionStage::Main.display_name("Run cache"), "Run cache");
    assert_eq!(
        ActionStage::Post.display_name("Run cache"),
        "Post Run cache"
    );
}
