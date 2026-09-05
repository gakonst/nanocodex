FROM node:22-alpine

WORKDIR /app
COPY relay.mjs /app/relay.mjs

ENV PORT=8080
EXPOSE 8080
USER node

CMD ["node", "/app/relay.mjs"]
