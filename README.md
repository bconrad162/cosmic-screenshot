# cosmic-screenshot

Utility for capturing screenshots via XDG Desktop Portal with optional post-capture annotation.

## Annotation mode

Pass `--annotate` to open the freshly captured screenshot in an annotation tool that supports brushes, highlighters, and basic shapes. By default we launch [`swappy`](https://github.com/jtheoof/swappy) so you can immediately mark up the image before it is saved or shared.

```
./target/release/cosmic-screenshot --annotate
```

When you select *Copy to clipboard* in the portal dialog we temporarily dump the clipboard image to a hidden file, open it in Swappy, and then push the annotated result back to the clipboard automatically (requires `wl-clipboard` on Wayland or `xclip` on X11).

## Build & run (Pop!_OS 24.04)

1. Install the prerequisites (Rust toolchain + swappy + clipboard helpers):
   ```
   sudo apt update
   sudo apt install rustup swappy wl-clipboard xclip
   rustup default stable
   ```
2. Build the binary:
   ```
   cargo build --release
   ```
3. Run it (add `--annotate` for the brush/highlight experience):
   ```
   ./target/release/cosmic-screenshot --annotate
   ```

Use `--save-dir ~/Pictures/Screenshots` if you want to bypass the interactive portal flow and save directly to a folder, or `--annotate-tool <command>` if you want to launch another swappy-compatible annotator.
