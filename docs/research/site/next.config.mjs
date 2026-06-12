import path from 'node:path';
import { fileURLToPath } from 'node:url';

const __dirname = path.dirname(fileURLToPath(import.meta.url));

/** @type {import('next').NextConfig} */
const nextConfig = {
  reactStrictMode: true,
  // Research site reads sibling research/*.html via fs at runtime — keep it as a
  // dynamic server-rendered app. No `output: 'export'` (which would force
  // everything to static at build time and break the runtime fs reads).
  output: undefined,
  // Pin tracing to research/site/ so Next doesn't accidentally walk up into
  // the parent grokrxiv workspace when collecting build traces.
  outputFileTracingRoot: __dirname,
};

export default nextConfig;
