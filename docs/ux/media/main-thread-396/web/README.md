# Full web app fixture capture

Captured from the complete `js/account` React app at `f517a4393b6c3ead1b2fe46d1b6470ec0e1611c1`, served locally with Vite. This is the actual app shell, navigation, route selection, composer and transcript renderer, rather than the previous isolated navigation component. Playwright uses a new ephemeral Chromium context. All `/v1` and `/api` responses are synthetic; non-local requests are aborted. No production traffic, login, model execution or real data is used.

| File | Caption |
| --- | --- |
| `01-entry.png` | Full web app with Main Thread entry, synthetic project list and generic selected agent. |
| `02-main-thread.png` | Main Thread opened via the navigation button. The seeded transcript explicitly labels its result as a fixture; no delegation occurred. |
| `03-project.png` | Demo Website selected from Projects, showing its synthetic coordinator transcript and normal composer. The generic header/recents title is “New agent” because the fixture omits summary titles. |
| `04-reuse.png` | Returning to Main Thread reuses the same canonical fixture URL/agent ID. |
| `navigation.webm` | Continuous 12.2-second browser recording of entry, Main Thread open, project selection and Main Thread reuse. No UI was composited. |

The result text is seeded history, not a result produced by backend execution. Research Notes is intentionally disabled because its fixture project has no coordinator. No create/register form exists in `MainThreadNavigation` at this commit. No create/register tool conversation was executed; those flows remain uncaptured on web.

`capture-log.json` records fixture request methods/paths and the final route, with zero browser console/page errors in the retained capture. The PUT responses deliberately return the same synthetic Main Thread identity; this demonstrates frontend reuse/navigation, not backend idempotency.

Reproduction: install the repository's pinned dependencies, launch `node js/account/node_modules/vite/bin/vite.js --config docs/ux/media/main-thread-396/web/vite.config.mjs`, then run `node docs/ux/media/main-thread-396/web/capture.mjs`. Set `CHROMIUM_PATH` if needed. The evidence environment reused the ignored matching node_modules prerequisite from the integrated worktree; UI source comes from the evidence worktree. The Vite config additionally allows that dependency source directory. Both existing source trees had identical captured UI files. The server binds only loopback port 5197.
