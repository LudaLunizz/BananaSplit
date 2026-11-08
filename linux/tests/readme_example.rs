//! The example in the README is a config file people copy: it must load.

use bananasplit::config::Config;

#[test]
fn the_readme_example_loads() {
    let readme = include_str!("../README.md");
    let start = readme.find("```ini\n[global]").expect("the README has the example") + "```ini\n".len();
    let block = &readme[start..start + readme[start..].find("```").unwrap()];
    let c = Config::parse(block).unwrap_or_else(|e| panic!("README example: {e:?}"));
    assert_eq!(c.buffer_size, 128);
    assert_eq!(c.strips[1].connect, ["^Firefox:output_F[LR]$"]);
    assert_eq!(c.recorder.folder, "~/Music/BananaSplit");
}
