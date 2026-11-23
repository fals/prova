FROM node:22
COPY --from=prova:dev /prova /usr/local/bin/prova
