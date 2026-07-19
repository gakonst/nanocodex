# syntax=docker/dockerfile:1.7

FROM ghcr.io/astral-sh/uv:0.9.5 AS uv

# This image is mounted read-only into every native task container. Keep all
# verifier dependencies here instead of constructing one derived image per task.
FROM debian:bookworm-slim

COPY --from=uv /uv /uvx /opt/nanocodex-verifier/bin/

RUN find /etc/apt -type f \( -name '*.list' -o -name '*.sources' \) \
        -exec sed -i 's|http://deb.debian.org|https://deb.debian.org|g' {} + && \
    if [ ! -s /etc/ssl/certs/ca-certificates.crt ]; then \
        DEBIAN_FRONTEND=noninteractive \
            apt-get -o Acquire::https::Verify-Peer=false update; \
        DEBIAN_FRONTEND=noninteractive \
            apt-get -o Acquire::https::Verify-Peer=false install \
                --yes --no-install-recommends ca-certificates; \
    fi && \
    DEBIAN_FRONTEND=noninteractive apt-get update && \
    DEBIAN_FRONTEND=noninteractive \
    apt-get install --yes --no-install-recommends \
        binutils \
        curl \
        expect \
        git \
        mtools \
        nodejs \
        openssh-client \
        primer3 \
        ripgrep \
        socat \
        sshpass \
        tesseract-ocr \
        vncsnapshot \
        vim && \
    rm -rf /var/lib/apt/lists/*

ENV UV_PYTHON_INSTALL_DIR=/opt/nanocodex-verifier/pythons
RUN --mount=type=cache,target=/root/.cache/uv \
    /opt/nanocodex-verifier/bin/uv python install 3.9 3.10 3.11 3.12 3.13 && \
    ln -s "$(/opt/nanocodex-verifier/bin/uv python find 3.11)" \
        /opt/nanocodex-verifier/python3.11 && \
    ln -s "$(/opt/nanocodex-verifier/bin/uv python find 3.13)" \
        /opt/nanocodex-verifier/python

# The common verifier stack is ABI-keyed because some wheels contain native
# extensions. The task's own interpreter still runs the canonical assertions.
RUN --mount=type=cache,target=/root/.cache/uv \
    for version in 3.9 3.10 3.11 3.12 3.13; do \
        /opt/nanocodex-verifier/bin/uv pip install --python "$version" \
            --target "/opt/nanocodex-verifier/base/$version" \
            pytest==8.4.1 \
            pytest-json-ctrf==0.3.5 \
            psutil==7.0.0 \
            rdflib==7.1.4 \
            requests==2.32.5; \
    done

# NumPy 2.3 requires Python 3.11 or newer. Keep every exact optional verifier
# environment isolated so it cannot replace an agent-visible task package.
RUN --mount=type=cache,target=/root/.cache/uv \
    for version in 3.11 3.12 3.13; do \
        /opt/nanocodex-verifier/bin/uv pip install --python "$version" \
            --target "/opt/nanocodex-verifier/numpy/$version" \
            numpy==2.3.0; \
    done
RUN --mount=type=cache,target=/root/.cache/uv \
    for version in 3.11 3.12 3.13; do \
        /opt/nanocodex-verifier/bin/uv pip install --python "$version" \
            --target "/opt/nanocodex-verifier/pov/$version" \
            pillow==11.1.0 \
            numpy==2.3.1 \
            scikit-image==0.25.0; \
    done
RUN --mount=type=cache,target=/root/.cache/uv \
    for version in 3.11 3.12 3.13; do \
        /opt/nanocodex-verifier/bin/uv pip install --python "$version" \
            --target "/opt/nanocodex-verifier/parquet/$version" \
            pandas==2.3.3 \
            pyarrow==22.0.0; \
    done
RUN --mount=type=cache,target=/root/.cache/uv \
    for version in 3.11 3.12 3.13; do \
        /opt/nanocodex-verifier/bin/uv pip install --python "$version" \
            --target "/opt/nanocodex-verifier/scientific/$version" \
            numpy==2.3.4 \
            pandas==2.3.3 \
            matplotlib==3.10.7 \
            scipy==1.16.3; \
    done
RUN --mount=type=cache,target=/root/.cache/uv \
    for version in 3.11 3.12 3.13; do \
        /opt/nanocodex-verifier/bin/uv pip install --python "$version" \
            --target "/opt/nanocodex-verifier/portfolio/$version" \
            numpy==2.3.2 \
            setuptools==78.1.1; \
        /opt/nanocodex-verifier/bin/uv pip install --python "$version" \
            --target "/opt/nanocodex-verifier/adaptive/$version" \
            numpy==2.3.3 \
            scipy==1.16.2; \
        /opt/nanocodex-verifier/bin/uv pip install --python "$version" \
            --target "/opt/nanocodex-verifier/mujoco/$version" \
            mujoco==3.3.5; \
    done

# These tasks start from bare Ubuntu images with no task-native Python. Share
# the large exact Torch wheel, and keep the pipeline-only Transformers stack in
# a separate overlay so the tensor verifier sees its canonical package set.
RUN --mount=type=cache,target=/root/.cache/uv \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/torch270/3.13 \
        torch==2.7.0 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/transformers455/3.13 \
        transformers==4.55.0

# A few canonical scripts use nearby exact pins. Keep them as small overlays;
# the common base remains shared by every verifier.
RUN --mount=type=cache,target=/root/.cache/uv \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/pytest842/3.13 \
        pytest==8.4.2 \
        pytest-json-ctrf==0.3.5 \
        psutil==7.0.0 \
        requests==2.32.5 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/requests2324/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        psutil==7.0.0 \
        requests==2.32.4 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/browser/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        selenium==4.35.0 \
        beautifulsoup4==4.13.5 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/filter-js/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        selenium==4.38.0 \
        bs4==0.0.2 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/datasets360/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        datasets==3.6.0 \
        tqdm==4.67.1 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/pillow1121/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        pillow==11.2.1 \
        numpy==2.3.1 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/bn/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        pandas==2.3.2 \
        scipy==1.16.1 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/setuptools809/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        setuptools==80.9.0 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/pytorch-cli/3.13 \
        --index https://download.pytorch.org/whl/cpu \
        --index-strategy unsafe-best-match \
        torch==2.7.1 \
        torchvision==0.22.1 \
        numpy==2.3.1 \
        opencv-python==4.11.0.86 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/install-windows-311/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        opencv-python==4.11.0.86 \
        numpy==2.3.1 \
        pytesseract==0.3.13 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.11 \
        --target /opt/nanocodex-verifier/fasttext/3.11 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        scikit-learn==1.7.0 \
        fasttext-wheel==0.9.2 \
        numpy==1.24.0 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.11 \
        --target /opt/nanocodex-verifier/sam-cell-seg/3.11 \
        --index https://download.pytorch.org/whl/cpu \
        --index-strategy unsafe-best-match \
        torch==2.5.1 \
        torchvision==0.20.1 \
        pandas==2.3.2 \
        timm==1.0.19 \
        opencv-python==4.12.0.88 \
        shapely==2.1.1 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        git+https://github.com/ChaoningZhang/MobileSAM.git@34bbbfdface3c18e5221aa7de6032d7220c6c6a1 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/pypi/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        pip==25.2 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/gitpython/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        gitpython==3.1.44 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/biopython/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        biopython==1.85 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/chess/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        chess==1.11.2 && \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/video/3.13 \
        pytest==8.4.1 \
        pytest-json-ctrf==0.3.5 \
        toml==0.10.2 \
        opencv-contrib-python==4.11.0.86

# OpenCV wheels dynamically load libGL. Export only its graphics dependency
# closure to task containers: exporting the toolbox's whole system library
# directory would also override the task's libc when its distribution is newer.
RUN DEBIAN_FRONTEND=noninteractive apt-get update && \
    DEBIAN_FRONTEND=noninteractive \
        apt-get install --yes --no-install-recommends libgl1 && \
    mkdir -p /opt/nanocodex-verifier/lib && \
    for library in \
        libGL.so.1 \
        libGLdispatch.so.0 \
        libGLX.so.0 \
        libX11.so.6 \
        libxcb.so.1 \
        libXau.so.6 \
        libXdmcp.so.6 \
        libbsd.so.0 \
        libmd.so.0 \
        libgthread-2.0.so.0 \
        libglib-2.0.so.0 \
        libpcre2-8.so.0; do \
        source=$(ldconfig -p | awk -v library="$library" \
            '$1 == library { print $NF; exit }'); \
        test -n "$source"; \
        cp -L "$source" "/opt/nanocodex-verifier/lib/$library"; \
    done && \
    rm -rf /var/lib/apt/lists/*

RUN --mount=type=cache,target=/root/.cache/uv \
    /opt/nanocodex-verifier/bin/uv pip install --python 3.13 \
        --target /opt/nanocodex-verifier/pytest834/3.13 \
        pytest==8.3.4 \
        pytest-json-ctrf==0.3.5

# Keep newly required system tools in an append-only layer so adding one
# verifier dependency does not invalidate the large Python/Torch overlays.
RUN DEBIAN_FRONTEND=noninteractive apt-get update && \
    DEBIAN_FRONTEND=noninteractive \
    apt-get install --yes --no-install-recommends \
        gcc \
        libc6-dev && \
    rm -rf /var/lib/apt/lists/*

# Preserve the task-ABI-first selection in bin/python for both spellings.
COPY bin/apt bin/python bin/toolbox-exec /opt/nanocodex-verifier/bin/
RUN chmod 0755 /opt/nanocodex-verifier/bin/* && \
    ln -s python /opt/nanocodex-verifier/bin/python3 && \
    for command in as curl expect gcc git ld mcopy mdir node readelf rg scp sftp socat ssh sshpass tesseract vim vncsnapshot oligotm primer3_core; do \
        ln -s toolbox-exec "/opt/nanocodex-verifier/bin/$command"; \
    done
