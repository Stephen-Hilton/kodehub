# Rust test layer: nothing beyond the core layer (bundles carry their test binary).
FROM kodehub/test-core:dev
LABEL kodehub.test-container-type="rust" kodehub.layers="core,rust"
