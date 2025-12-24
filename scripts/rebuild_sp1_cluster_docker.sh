#!/bin/bash
# Script to rebuild SP1 Cluster Docker images without cache
# This ensures the proto/google/protobuf/empty.proto file is included

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ERE_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"

echo "Rebuilding SP1 Cluster Docker images without cache..."
echo "ERE root: $ERE_ROOT"

cd "$ERE_ROOT"

# Remove existing images to force rebuild
echo "Removing existing SP1 Cluster images..."
docker rmi ere-base-sp1-cluster:0.0.14-7b8bb6a ere-base-sp1-cluster:latest 2>/dev/null || true
docker rmi ere-compiler-sp1-cluster:0.0.14-7b8bb6a ere-compiler-sp1-cluster:latest 2>/dev/null || true
docker rmi ere-server-sp1-cluster:0.0.14-7b8bb6a ere-server-sp1-cluster:latest 2>/dev/null || true

# Rebuild base image without cache
echo ""
echo "Building base image (this may take a while)..."
docker build \
    --no-cache \
    --file docker/sp1-cluster/Dockerfile.base \
    --tag ere-base-sp1-cluster:0.0.14-7b8bb6a \
    --tag ere-base-sp1-cluster:latest \
    .

# Rebuild compiler image without cache
echo ""
echo "Building compiler image (this may take a while)..."
docker build \
    --no-cache \
    --file docker/sp1-cluster/Dockerfile.compiler \
    --build-arg BASE_ZKVM_IMAGE=ere-base-sp1-cluster:latest \
    --tag ere-compiler-sp1-cluster:0.0.14-7b8bb6a \
    --tag ere-compiler-sp1-cluster:latest \
    .

# Rebuild server image without cache
echo ""
echo "Building server image (this may take a while)..."
docker build \
    --no-cache \
    --file docker/sp1-cluster/Dockerfile.server \
    --build-arg BASE_ZKVM_IMAGE=ere-base-sp1-cluster:latest \
    --tag ere-server-sp1-cluster:0.0.14-7b8bb6a \
    --tag ere-server-sp1-cluster:latest \
    .

echo ""
echo "✅ SP1 Cluster Docker images rebuilt successfully!"
echo ""
echo "Images:"
docker images | grep "sp1-cluster" | head -10
