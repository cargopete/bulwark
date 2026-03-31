# ══════════════════════════════════════════════════════════════════════
# Stage 1: Build the Rust CLI
# ══════════════════════════════════════════════════════════════════════
FROM rust:1.85-bookworm AS builder

WORKDIR /build
COPY Cargo.toml Cargo.lock* ./
COPY src/ src/

# Build release binary
RUN cargo build --release && strip target/release/bulwark

# ══════════════════════════════════════════════════════════════════════
# Stage 2: Runtime image with all audit tooling
# ══════════════════════════════════════════════════════════════════════
FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive

# ── System deps ──────────────────────────────────────────────────────
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential \
    ca-certificates \
    curl \
    git \
    jq \
    python3 \
    python3-pip \
    python3-venv \
    ripgrep \
    fd-find \
    sudo \
    unzip \
    wget \
    && rm -rf /var/lib/apt/lists/*

# Symlink fd (Ubuntu packages it as fdfind)
RUN ln -sf /usr/bin/fdfind /usr/bin/fd

# ── Node.js 22 ───────────────────────────────────────────────────────
RUN curl -fsSL https://deb.nodesource.com/setup_22.x | bash - \
    && apt-get install -y nodejs \
    && npm install -g pnpm@9 \
    && rm -rf /var/lib/apt/lists/*

# ── Python tools (Slither + optionally Mythril) ─────────────────────
RUN python3 -m venv /opt/solidity-tools \
    && /opt/solidity-tools/bin/pip install --no-cache-dir \
        slither-analyzer \
    && ln -sf /opt/solidity-tools/bin/slither /usr/local/bin/slither

# Mythril is optional — has problematic deps on some platforms
RUN /opt/solidity-tools/bin/pip install --no-cache-dir mythril \
    && ln -sf /opt/solidity-tools/bin/myth /usr/local/bin/myth \
    || echo "WARNING: Mythril install failed (optional) — continuing without it"

# ── Foundry (Forge, Cast, Anvil) ────────────────────────────────────
RUN curl -L https://foundry.paradigm.xyz | bash \
    && /root/.foundry/bin/foundryup
ENV PATH="/root/.foundry/bin:${PATH}"

# ── solc-select (manage Solidity compiler versions) ──────────────────
RUN /opt/solidity-tools/bin/pip install --no-cache-dir solc-select \
    && ln -sf /opt/solidity-tools/bin/solc-select /usr/local/bin/solc-select \
    && ln -sf /opt/solidity-tools/bin/solc /usr/local/bin/solc \
    && solc-select install 0.8.27 \
    && solc-select use 0.8.27

# ── Create audit user (non-root for safety) ──────────────────────────
RUN useradd -m -s /bin/bash auditor \
    && echo "auditor ALL=(ALL) NOPASSWD:ALL" >> /etc/sudoers
USER auditor
WORKDIR /home/auditor

# ── Foundry for auditor user ─────────────────────────────────────────
RUN curl -L https://foundry.paradigm.xyz | bash \
    && /home/auditor/.foundry/bin/foundryup
ENV PATH="/home/auditor/.foundry/bin:${PATH}"

# ── Claude Code (native installer, as auditor user) ─────────────────
RUN curl -fsSL https://claude.ai/install.sh | bash
ENV PATH="/home/auditor/.local/bin:${PATH}"

# ── Copy Bulwark CLI binary from builder ──────────────────────────────
COPY --from=builder --chown=auditor:auditor /build/target/release/bulwark /usr/local/bin/bulwark

# ── Directory structure ──────────────────────────────────────────────
RUN mkdir -p \
    /home/auditor/.claude \
    /home/auditor/.claude/commands \
    /home/auditor/tools \
    /home/auditor/audits \
    /home/auditor/context \
    /home/auditor/pipeline/lib \
    /home/auditor/prompts \
    /home/auditor/schemas

# ── Copy config, scripts, and pipeline assets ────────────────────────
COPY --chown=auditor:auditor config/settings.json /home/auditor/.claude/settings.json
COPY --chown=auditor:auditor config/settings.json /home/auditor/.bulwark-settings.json
COPY --chown=auditor:auditor config/CLAUDE.md /home/auditor/.claude/CLAUDE.md
COPY --chown=auditor:auditor context/ /home/auditor/context/
COPY --chown=auditor:auditor scripts/ /home/auditor/scripts/
COPY --chown=auditor:auditor pipeline/ /home/auditor/pipeline/
COPY --chown=auditor:auditor prompts/ /home/auditor/prompts/
COPY --chown=auditor:auditor schemas/ /home/auditor/schemas/
COPY --chown=auditor:auditor bulwark.toml /home/auditor/bulwark.toml
RUN chmod +x /home/auditor/scripts/*.sh \
    && chmod +x /home/auditor/pipeline/*.sh \
    && chmod +x /home/auditor/pipeline/lib/*.sh

ENV BULWARK_ROOT="/home/auditor"
ENV BULWARK_CONTAINER="1"

ENTRYPOINT ["/home/auditor/scripts/entrypoint.sh"]
CMD ["bash"]
