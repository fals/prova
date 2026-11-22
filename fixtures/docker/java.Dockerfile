FROM maven:3.9-eclipse-temurin-21
COPY --from=toe:dev /toe /usr/local/bin/toe
