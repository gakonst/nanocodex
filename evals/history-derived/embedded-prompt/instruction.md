Move the system prompt out of Rust string constants into one compile-time
embedded Markdown file, following `README.md`. Preserve the public API and exact
prompt bytes. Run the tests.
