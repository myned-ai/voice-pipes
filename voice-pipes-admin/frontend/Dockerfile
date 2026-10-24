# Multi-stage: `prod` (default) serves a minified static build via nginx;
# the `dev` target runs the Vite dev server with HMR (build with
# `--target dev` or set `target: dev` in a compose override).
FROM node:22-alpine AS base
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci
COPY . .

FROM base AS dev
EXPOSE 3000
CMD ["npm", "run", "dev", "--", "--host", "0.0.0.0", "--port", "3000"]

FROM base AS build
# Vite bakes VITE_* at build time. The source defaults (localhost:8000 API,
# localhost:8800 WS) match the standard stack, so no args are needed there;
# override for non-local deployments.
ARG VITE_API_URL
ARG VITE_WS_URL
RUN npm run build

# Playground Extensions: at runtime, bind-mount
# extension bundles at /usr/share/nginx/html/ext/ and the manifest at
# /usr/share/nginx/html/playground-extensions.json — no rebuild required.
# The unprivileged nginx image runs as uid 101 with its pid and cache
# under /tmp; our conf listens on 3000 (> 1024), so nothing else changes.
FROM nginxinc/nginx-unprivileged:1.27-alpine AS prod
COPY --from=build /app/dist /usr/share/nginx/html
COPY nginx.conf /etc/nginx/conf.d/default.conf
COPY security-headers.conf /etc/nginx/snippets/security-headers.conf
EXPOSE 3000
# busybox wget ships in the alpine base; the index needs no token.
HEALTHCHECK --interval=15s --timeout=5s --start-period=10s --retries=3 \
    CMD ["wget", "-q", "-O", "/dev/null", "http://127.0.0.1:3000/"]
