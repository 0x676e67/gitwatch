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

The guide describes version 0.4. Historical demonstration screenshots remain in `assets/screenshots/` for existing external links, but are not embedded because they show an older interface. Replacements must use isolated demonstration data, never personal tasks or repositories.

## Publishing

Pull requests targeting `gh-pages` build and validate without deploying. After review and merge, pushes to `gh-pages` build and publish through the `github-pages` environment.

One-time repository setup: set **Settings → Pages → Source** to **GitHub Actions**, and restrict the `github-pages` environment to the `gh-pages` branch. The first documentation PR requires the project owner's review before merge and before enabling publication. Merge the site before the README PR so its documentation links work.

Only `_site/` is uploaded. Research notes and local backup data are never part of the site. Keep the displayed version and migration instructions aligned with the release. The old TUI page redirects to the upgrade guide.
