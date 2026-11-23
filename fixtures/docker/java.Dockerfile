FROM maven:3.9-eclipse-temurin-21
COPY --from=prova:dev /prova /usr/local/bin/prova
