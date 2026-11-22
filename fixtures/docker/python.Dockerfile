FROM python:3.13
RUN pip install --no-cache-dir pytest==8.3.4 pytest-benchmark==5.1.0 pytest-reportlog==0.4.0
COPY --from=toe:dev /toe /usr/local/bin/toe
