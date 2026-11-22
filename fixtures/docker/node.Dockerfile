FROM node:22
COPY --from=toe:dev /toe /usr/local/bin/toe
