# gitwatch documentation

English and Simplified Chinese documentation lives on `gh-pages`, separate from the Rust application on `main`.

## Preview

Python 3.11 or newer is sufficient. No third-party packages are required.

```sh
python build.py
python check.py
python -m http.server 8000 --directory _site --bind 127.0.0.1
```

Open http://127.0.0.1:8000/en/index.html or http://127.0.0.1:8000/zh-CN/index.html.

Edit page bodies in `content/en/` and `content/zh-CN/`. Titles, descriptions and navigation order are in `pages.json`. The template is in `build.py`; shared styles and progressive enhancements are in `assets/`. Generated `_site/` files are not committed.

Keep both languages in sync, including section IDs. Use relative links so the site works under the GitHub Pages project path and in a local preview. Navigation and all documentation remain usable without JavaScript; JavaScript only collapses the mobile menu and adds copy buttons.

## Screenshots

Images in `assets/screenshots/` are unedited captures of the Windows desktop application based on source commit `f06d044ee5cd7d24d9b4ef1f16efa3e50f50eccd`, with the tray event wakeup fix applied, using dedicated demonstration repositories. They show the development version, not v0.1.0.

Before replacing screenshots, close the running app, back up and verify any real application data, and use a clean demonstration store. Do not include personal tasks, paths, remote credentials or private repository contents. Restore the original data afterwards. Capture both interface languages and update the HTML image dimensions to match.

## Publishing

Pull requests targeting `gh-pages` build and validate without deploying. After review and merge, pushes to `gh-pages` build and publish through the `github-pages` environment.

One-time repository setup: set **Settings → Pages → Source** to **GitHub Actions**, and restrict the `github-pages` environment to the `gh-pages` branch. The first documentation PR requires the project owner's review before merge and before enabling publication. Merge the site before the README PR so its documentation links work.

Only `_site/` is uploaded. Research notes and local backup data are never part of the site. Keep the development-version notice until a release includes the documented features.
