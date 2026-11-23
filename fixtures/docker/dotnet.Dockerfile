FROM mcr.microsoft.com/dotnet/sdk:9.0
COPY --from=prova:dev /prova /usr/local/bin/prova
