# Embedded prompt cleanup

The prompt currently lives in stitched Rust constants. Move all prompt prose
into exactly one Markdown file under `src/` and embed it at compile time. Both
public functions must use the same embedded source.

- Preserve the exact bytes returned by `system_prompt()`.
- `diagnostic_prompt()` must return the same prompt without another copy.
- Do not read files at runtime.
- Delete obsolete prompt constants and stitching logic.
- Keep the public function signatures unchanged.
