FROM python:3.12-slim-bookworm

RUN python -m pip install --no-cache-dir \
    numpy==2.3.2 \
    pandas==2.3.1 \
    scipy==1.16.1 \
    scikit-learn==1.7.1 \
    statsmodels==0.14.5

WORKDIR /workspace
COPY data_files/ /workspace/data_files/
