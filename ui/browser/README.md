# DSM container regression

Start the fixture server with `pnpm dev --host 127.0.0.1 --port 15144`.
`browser/dsm-layout.html` mounts the actual app and DSM library components
inside a clipped, independently sized window body with a 40 px chrome fixture.
It uses recorded API fixtures; it does not call a NAS.

Run `check-layout.mjs` through the ego-browser skill in the task's existing
space. Prepend a `layoutConfig` constant with `spaceId`, `page`, `baseUrl` and
an absolute `artifactDir`, then pass the combined source to
`ego-browser nodejs`. The page must already belong to that task space.

The matrix covers Overview and Explore status details in nine DSM locales, 700×560, 800×560, 1100×720 and 1350×850 windows, and 125%
CSS zoom. It asserts panel/header/footer bounds relative to AppRoot, status
visible tooltip bounds on all four edges after hover and focus, real wheel scrolling and PageDown scrolling from a normal
checkbox control. Long displayed names exercise wrapping. It never clicks Scan.
Overview also exercises the scan dialog. Screenshots and JSON geometry are saved for review. CSS zoom models scaled
content; it does not claim to test the browser's native zoom menu.

A real DSM acceptance check must additionally use the installed bundle,
actual AppWindow body and existing report, record its hashes, and leave scans
and cleanup untouched. jsdom style checks alone do not establish clipping or
scroll behavior.

# Full-page (Portal/standalone) presentation

`browser/portal-layout.html` mounts the app through `dsmBundle` the way a
standalone launch does: no viewport meta on the host page, the shell's
`fullPage` on, the package icon from `?icon=`. Open it at phone, window and
desktop widths to see mobile, window and web; `?fullPage=0` shows the DSM-window behavior.

For the iPhone Safari regression, run `node browser/check-iphone-full-page.mjs`
from `ui/` with the root npm dependencies installed (`playwright-core`) and its
WebKit browser available. Set `ARTIFACT_DIR` to the screenshot/JSON directory;
`BASE_URL` defaults to `http://127.0.0.1:15144`.
The `?prepareFullPage=1` fixture first sizes the host at the initial ~980px,
then sets device-width before mounting the real App, like DSM prepareFullPage.
The test uses WebKit's iPhone 15 and suppresses trusted resize events, requiring
the UI library's synthetic resize to refit that host. It checks phone width,
no page overflow and `data-ui-layout-mode="mobile"`. This models iOS timing;
it does not replace a real iPhone or NAS acceptance check.
