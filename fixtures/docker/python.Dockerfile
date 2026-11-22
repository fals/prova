FROM python:3.13
RUN pip install --no-cache-dir pytest==8.3.4
COPY --from=toe:dev /toe /usr/local/bin/toe
