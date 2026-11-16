# Vendored UI library

`htm-preact-3.1.1.mjs` is `preact/standalone.module.js` from the npm package `htm@3.1.1`, copied byte for byte (Preact bundled with htm; exports `h`, `html`, `render`, `Component`, `createContext` and the hooks). License: Apache-2.0 (`htm-3.1.1.LICENSE`); Preact itself is MIT.

- Tarball integrity (npm registry): `sha512-983Vyg8NwUE7JkZ6NmOqpCZ+sh1bKv2iYTlUkzlWmA5JD2acKoxd4KVxbMmxX/85mtfdnDmTFoNKcg5DGAvxNQ==`
- File sha256: `72284e8e9079c87817145df1110f74e8a2aa040b2fc384922e18dfcb46fc1fd7`
- Audited 2026-11-15: no `eval` or `new Function`; templates are tagged templates, compatible with the strict CSP.

Never edit this file. To upgrade, replace it with the new release's file, update the name, both checksums and the license, and rerun the web tests. UI code must not use `dangerouslySetInnerHTML`.
