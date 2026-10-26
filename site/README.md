# Nullamp website

A static site (plain HTML, CSS and a little JavaScript, no build step, no external requests). `.github/workflows/pages.yml` publishes this folder to GitHub Pages on every push to `main` that touches it.

## Publish once

In the repository on GitHub: **Settings > Pages > Build and deployment > Source: GitHub Actions**. The site then appears at `https://jbrunemann.github.io/nullamp/`.

## Preview locally

```sh
python3 -m http.server -d site 8000   # then open http://localhost:8000
```

## Screenshots

`screenshots/main.png` is a copy of `docs/images/nullamp-main.png`. The other four are rendered from the real UI with made-up music (synthesized, tagged files with generated covers, so no real artists or releases appear):

```sh
site/tools/make-showcase.sh "/Users/Shared/Sunday Sets"      # about two minutes, 85 MB
NULLAMP_SHOWCASE="/Users/Shared/Sunday Sets" cargo test -p nullamp render_site_screenshots -- --ignored --test-threads=1
cp target/screenshots/site/*.png site/screenshots/
```

Use a neutral folder like the one above: the inspector shows the path of the file. To use your own music instead, save any PNG over `setlist.png`, `identify.png`, `inspector.png` or `dj-software.png`. The page sizes them by the width and height attributes in `index.html`, so update those if the proportions change.
