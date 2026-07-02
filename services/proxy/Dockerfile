FROM node:24-alpine AS build

WORKDIR /app

COPY . .

RUN corepack enable && pnpm install --frozen-lockfile
RUN pnpm exec esbuild services/proxy/server.ts \
	--bundle \
	--platform=node \
	--format=esm \
	--target=node24 \
	--outfile=/app/dist/server.mjs

FROM node:24-alpine

ENV NODE_ENV=production
ENV PORT=8080

WORKDIR /app

COPY --from=build /app/dist/server.mjs ./server.mjs

EXPOSE 8080

CMD ["node", "server.mjs"]
