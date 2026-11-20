# Duet website

The homepage for Duet: one static page written in Rust with [Dioxus](https://dioxuslabs.com) 0.7
and compiled to WebAssembly. No external CDNs, fonts or trackers; everything it loads is in
`assets/`.

This crate is standalone (it has its own empty `[workspace]`), so it never affects the app's
build.

## Build and preview

Needs the `wasm32-unknown-unknown` target and the Dioxus CLI 0.7:

```sh
rustup target add wasm32-unknown-unknown
cargo install dioxus-cli --version 0.7.10 --locked

cd site
dx serve --platform web                                   # live-reloading dev server
dx bundle --platform web --release --out-dir dist         # static build -> dist/public/
python3 -m http.server -d dist/public 8080                # preview the build at http://localhost:8080
```

`dist/public/` is the whole site: `index.html` plus hashed files in `assets/` (the `.wasm`,
its JS loader, CSS and images). Serve it from the domain root (or rebuild with
`--base-path <path>` to host it under a sub-path).

The download buttons point at
`https://github.com/EricMarcantonio/duet/releases/latest/download/Duet-{macOS.zip,Windows.zip,Linux.tar.gz}`,
the files `.github/workflows/release.yml` attaches to each release. Keep the names in sync.

## Deploying (GitHub Pages, live)

<https://ericmarcantonio.github.io/duet/> is built and published by
[`.github/workflows/pages.yml`](../.github/workflows/pages.yml) on every push that touches
`site/` (or on demand from the Actions tab → Website → Run workflow). It builds with
`--base-path /duet`, which the asset resolver bakes into every URL, and uploads
`site/dist/public` as the Pages artifact. Pages is set to "GitHub Actions" as its source
(Settings › Pages), so nothing is committed to a `gh-pages` branch.

The Dioxus CLI is installed from crates.io on the first run and cached afterwards
(keyed on the CLI version), so the first deploy takes a few minutes and later ones about a
minute.

### Moving to a custom domain (marcantonio.dev, on Cloudflare DNS)

1. Add a `CNAME` file to this folder containing just the hostname, e.g.
   `echo marcantonio.dev > site/CNAME`, and push. The workflow then builds for the domain
   root instead of `/duet/`.
2. In Cloudflare DNS for `marcantonio.dev`, add records for GitHub Pages, **Proxy status:
   DNS only** (grey cloud — Pages issues its own certificate):
   - apex: `A @ 185.199.108.153`, `185.199.109.153`, `185.199.110.153`, `185.199.111.153`
   - `www`: `CNAME www ericmarcantonio.github.io`
3. In the repo: Settings › Pages › Custom domain → `marcantonio.dev`, then tick
   **Enforce HTTPS** once the certificate is issued.

Once the domain is set, `<owner>.github.io/<repo>/` redirects to it, so the `/duet/` build
base path must not be used with a custom domain — hence the `CNAME` switch in the workflow.

### Alternatives

Vercel works too (`vercel.json` here skips Vercel's build and serves a prebuilt `dist/public`:
`dx bundle --platform web --release --out-dir dist` then `npx vercel deploy --prod`), as does
Cloudflare Pages or any static host — the output is plain files.
