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

## Deploying (not set up yet)

Any static host works. Some options:

### Vercel, prebuilt (simplest)

`vercel.json` tells Vercel to skip building and serve `dist/public` as-is (with long-lived
caching for the hashed `assets/`).

```sh
cd site
dx bundle --platform web --release --out-dir dist
npx vercel link                 # once: create/link the Vercel project, root directory = site
npx vercel deploy --prod        # uploads dist/public
```

(`vercel build` + `vercel deploy --prebuilt` works too if you prefer Vercel's build-output flow.)

### Vercel, from Git

Vercel's build image doesn't ship the Rust toolchain, so either:

- commit the build output (e.g. build into a `public/` folder that isn't ignored) and point the
  project's output directory at it; or
- build in a GitHub Action and deploy with the Vercel CLI, using `VERCEL_TOKEN`,
  `VERCEL_ORG_ID` and `VERCEL_PROJECT_ID` secrets:

  ```yaml
  - uses: dtolnay/rust-toolchain@stable
    with: { targets: wasm32-unknown-unknown }
  - run: cargo install dioxus-cli --version 0.7.10 --locked
  - run: dx bundle --platform web --release --out-dir dist
    working-directory: site
  - run: npx vercel deploy --prod --token "$VERCEL_TOKEN"
    working-directory: site
  ```

GitHub Pages or Cloudflare Pages work the same way: upload `dist/public`.

### Custom domain (marcantonio.dev on Cloudflare DNS)

1. In the Vercel project: Settings › Domains › add the domain (for example `duet.marcantonio.dev`).
2. In Cloudflare DNS for `marcantonio.dev`, add a record:
   - subdomain: `CNAME duet → cname.vercel-dns.com`, **Proxy status: DNS only** (grey cloud);
   - apex (`marcantonio.dev` itself): Cloudflare flattens a CNAME at the root, or use the
     `A` record Vercel shows (`76.76.21.21`), also DNS only.
3. Wait for Vercel to verify the domain and issue the certificate.

Keep the Cloudflare proxy off: Vercel issues and renews the TLS certificate itself, and
proxying through Cloudflare gets in the way of that (and of Vercel's caching).
