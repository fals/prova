FROM mcr.microsoft.com/dotnet/sdk:9.0
COPY --from=toe:dev /toe /usr/local/bin/toe
