# Core test container layer (spec §5.4.1): a small Debian userland with a shell and the
# kodehub-worker entrypoint. Rust test bundles need nothing more (they carry their compiled
# binary); Python bundles carry their package and vendored dependencies, so the interpreter
# here is the only Python layer they need.
FROM kodehub/kodehub:dev AS kodehub
FROM python:3.12-slim
COPY --from=kodehub /usr/local/bin/kodehub-worker /usr/local/bin/kodehub-worker
RUN useradd -m -u 10001 kodehub
USER kodehub
WORKDIR /work
ENV KODEHUB_HOME=/work/kode
ENTRYPOINT []
