FROM nanocodex-computer:integration
USER root
RUN apk add --no-cache python3 python3-tkinter xvfb xauth font-dejavu
COPY live.py /test/live.py
ENTRYPOINT ["python3", "/test/live.py"]
