# syntax=docker/dockerfile:1.7

ARG BASE_IMAGE
FROM ghcr.io/astral-sh/uv:0.9.5 AS uv

FROM ${BASE_IMAGE}
COPY --from=uv /uv /uvx /usr/local/bin/
# Canonical tests may repeat this package check inside the trial. Keep the
# indexes and dependencies in the cached verifier layer so that check is local.
RUN find /etc/apt -type f \( -name '*.list' -o -name '*.sources' \) \
        -exec sed -i 's|http://ports.ubuntu.com|https://ports.ubuntu.com|g' {} + && \
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
        curl \
        expect \
        git \
        openssh-client \
        python3 \
        python-is-python3 \
        vim && \
    if apt-cache policy python3-distutils | \
            grep -Eq 'Candidate: [^ (]'; then \
        DEBIAN_FRONTEND=noninteractive \
        apt-get install --yes --no-install-recommends python3-distutils; \
    fi
RUN uv pip install --system --break-system-packages --no-cache \
    pytest==8.4.1 \
    pytest-json-ctrf==0.3.5 \
    requests==2.32.5
# Keep new task-specific system tools in an append-only layer so existing apt
# and Python dependency layers remain reusable across verifier overlays.
RUN DEBIAN_FRONTEND=noninteractive \
    apt-get install --yes --no-install-recommends primer3
RUN uv pip install --system --break-system-packages --no-cache \
    rdflib==7.1.4
# NumPy 2.3 requires Python 3.11. Older task images do not need it unless their
# canonical verifier asks for it, and those verifier tasks use newer Python.
RUN if python -c 'import sys; sys.exit(sys.version_info < (3, 11))'; then \
        uv pip install --system --break-system-packages --no-cache \
            numpy==2.3.0; \
    fi
# POV-Ray's canonical image comparison adds this exact scientific-image stack.
# Keep verifier-only Python packages outside the system interpreter: tasks may
# assert the versions already present in their agent environment.
RUN if python -c 'import sys; sys.exit(sys.version_info < (3, 11))'; then \
        uv pip install --target /opt/harness-verifier/pov --no-cache \
            pillow==11.1.0 \
            numpy==2.3.1 \
            scikit-image==0.25.0; \
    fi
# Canonical tests sometimes reinstall an already pinned package only to undo
# possible agent edits. Preserve that guard without repeating expensive TeX
# format generation when the package files are still byte-for-byte pristine.
COPY bin/apt /opt/harness-verifier/bin/apt
RUN chmod 0755 /opt/harness-verifier/bin/apt && \
    if [ -s /var/lib/dpkg/info/texlive-latex-base.md5sums ]; then \
        mkdir -p /opt/harness-verifier/manifests && \
        while read -r digest path; do \
            if [ -f "/$path" ]; then \
                printf '%s  %s\n' "$digest" "$path"; \
            fi; \
        done < /var/lib/dpkg/info/texlive-latex-base.md5sums \
            > /opt/harness-verifier/manifests/texlive-latex-base.md5sums && \
        find /var/lib/texmf -type f \
            \( -name pdflatex.fmt -o -name pdftex.map -o -name ls-R \) \
            -exec md5sum {} + | sort -k2 \
            > /opt/harness-verifier/manifests/texlive-generated.md5sums; \
    fi
