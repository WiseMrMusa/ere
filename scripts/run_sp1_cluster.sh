#!/bin/bash
# Script to start SP1 Cluster infrastructure and run benchmarks with ere
# 
# Usage:
#   ./scripts/run_sp1_cluster.sh start [num_gpus]  - Start the cluster with N GPUs (default: all)
#   ./scripts/run_sp1_cluster.sh stop              - Stop the cluster
#   ./scripts/run_sp1_cluster.sh status            - Check cluster status
#   ./scripts/run_sp1_cluster.sh test              - Run a test benchmark
#   ./scripts/run_sp1_cluster.sh benchmark         - Run zkevm benchmark
#   ./scripts/run_sp1_cluster.sh logs [service]    - View logs

set -e

# Configuration
SP1_CLUSTER_REPO="${SP1_CLUSTER_REPO:-/root/sp1-cluster-infra}"
SP1_CLUSTER_ENDPOINT="${SP1_CLUSTER_ENDPOINT:-http://127.0.0.1:50051}"
SP1_CLUSTER_REDIS_URL="${SP1_CLUSTER_REDIS_URL:-redis://:redispassword@127.0.0.1:6379/0}"

# Export for use by ere
export SP1_CLUSTER_ENDPOINT
export SP1_CLUSTER_REDIS_URL

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

info() {
    echo -e "${GREEN}[INFO]${NC} $1"
}

warn() {
    echo -e "${YELLOW}[WARN]${NC} $1"
}

error() {
    echo -e "${RED}[ERROR]${NC} $1"
    exit 1
}

check_sp1_cluster_repo() {
    if [ ! -d "$SP1_CLUSTER_REPO/infra" ]; then
        error "SP1 Cluster repo not found at $SP1_CLUSTER_REPO. Clone it first:
  git clone https://github.com/succinctlabs/sp1-cluster.git $SP1_CLUSTER_REPO"
    fi
}

start_cluster() {
    local num_gpus="${1:-4}"
    check_sp1_cluster_repo
    
    info "Starting SP1 Cluster with $num_gpus GPUs..."
    cd "$SP1_CLUSTER_REPO/infra"
    
    # Start the cluster
    if [ "$num_gpus" -eq 0 ]; then
        info "Starting in CPU-only mode..."
        docker compose up -d mixed 2>&1 | grep -v "level=warning" || true
    else
        info "Starting with $num_gpus GPU nodes..."
        local gpu_services=""
        for i in $(seq 0 $((num_gpus - 1))); do
            gpu_services="$gpu_services gpu$i"
        done
        docker compose up -d $gpu_services 2>&1 | grep -v "level=warning" || true
    fi
    
    # Wait for services to be ready
    info "Waiting for services to be ready..."
    sleep 5
    
    # Expose ports for external access
    info "Exposing API port 50051..."
    docker compose port api 50051 2>/dev/null || true
    
    # Check status
    status_cluster
    
    info "SP1 Cluster is ready!"
    info "  Endpoint: $SP1_CLUSTER_ENDPOINT"
    info "  Redis:    $SP1_CLUSTER_REDIS_URL"
    info ""
    info "Environment variables (add to your shell or .env):"
    echo "export SP1_CLUSTER_ENDPOINT=$SP1_CLUSTER_ENDPOINT"
    echo "export SP1_CLUSTER_REDIS_URL=$SP1_CLUSTER_REDIS_URL"
}

stop_cluster() {
    check_sp1_cluster_repo
    info "Stopping SP1 Cluster..."
    cd "$SP1_CLUSTER_REPO/infra"
    docker compose down 2>&1 | grep -v "level=warning" || true
    info "SP1 Cluster stopped."
}

status_cluster() {
    check_sp1_cluster_repo
    info "SP1 Cluster status:"
    cd "$SP1_CLUSTER_REPO/infra"
    docker compose ps 2>&1 | grep -v "level=warning" || true
}

logs_cluster() {
    local service="${1:-}"
    check_sp1_cluster_repo
    cd "$SP1_CLUSTER_REPO/infra"
    if [ -n "$service" ]; then
        docker compose logs -f "$service" 2>&1 | grep -v "level=warning"
    else
        docker compose logs -f 2>&1 | grep -v "level=warning"
    fi
}

test_cluster() {
    check_sp1_cluster_repo
    info "Running SP1 Cluster test benchmark (20M cycles Fibonacci)..."
    cd "$SP1_CLUSTER_REPO/infra"
    
    docker run --rm --network=infra_default \
        ghcr.io/succinctlabs/sp1-cluster:base-latest \
        /cli bench fibonacci 20 \
        --cluster-rpc http://api:50051 \
        --redis-nodes redis://:redispassword@redis:6379/0
}

run_benchmark() {
    info "Running zkevm benchmark with SP1 Cluster..."
    
    # Check that SP1 Cluster is running
    if ! docker ps | grep -q "infra-api-1"; then
        error "SP1 Cluster is not running. Start it first with: $0 start"
    fi
    
    # Get the API container's IP for external access
    local api_ip=$(docker inspect -f '{{range.NetworkSettings.Networks}}{{.IPAddress}}{{end}}' infra-api-1 2>/dev/null || echo "127.0.0.1")
    local redis_ip=$(docker inspect -f '{{range.NetworkSettings.Networks}}{{.IPAddress}}{{end}}' infra-redis-1 2>/dev/null || echo "127.0.0.1")
    
    export SP1_CLUSTER_ENDPOINT="http://${api_ip}:50051"
    export SP1_CLUSTER_REDIS_URL="redis://:redispassword@${redis_ip}:6379/0"
    
    info "Using cluster at: $SP1_CLUSTER_ENDPOINT"
    info "Using redis at: $SP1_CLUSTER_REDIS_URL"
    
    # Run the benchmark
    cd "$(dirname "$0")/../.."
    if [ -d "zkevm-benchmark-workload" ]; then
        cd zkevm-benchmark-workload
        cargo run --release --package ere-hosts -- \
            --resource cluster \
            --cluster-endpoint "$SP1_CLUSTER_ENDPOINT" \
            --cluster-redis-url "$SP1_CLUSTER_REDIS_URL" \
            --zkvms sp1-cluster \
            "$@"
    else
        error "zkevm-benchmark-workload not found. Run from ere root directory."
    fi
}

# Main
case "${1:-help}" in
    start)
        start_cluster "${2:-4}"
        ;;
    stop)
        stop_cluster
        ;;
    status)
        status_cluster
        ;;
    logs)
        logs_cluster "$2"
        ;;
    test)
        test_cluster
        ;;
    benchmark)
        shift
        run_benchmark "$@"
        ;;
    help|*)
        echo "SP1 Cluster Management Script for ere"
        echo ""
        echo "Usage: $0 <command> [options]"
        echo ""
        echo "Commands:"
        echo "  start [num_gpus]  Start the cluster with N GPUs (default: 4)"
        echo "  stop              Stop the cluster"
        echo "  status            Check cluster status"
        echo "  logs [service]    View logs (optionally for specific service)"
        echo "  test              Run a test benchmark via SP1 Cluster CLI"
        echo "  benchmark [args]  Run zkevm benchmark with ere"
        echo ""
        echo "Environment Variables:"
        echo "  SP1_CLUSTER_REPO      Path to sp1-cluster repo (default: /root/sp1-cluster-infra)"
        echo "  SP1_CLUSTER_ENDPOINT  Cluster API endpoint (default: http://127.0.0.1:50051)"
        echo "  SP1_CLUSTER_REDIS_URL Redis URL (default: redis://:redispassword@127.0.0.1:6379/0)"
        echo ""
        echo "Examples:"
        echo "  $0 start 4              # Start with 4 GPUs"
        echo "  $0 test                 # Run Fibonacci benchmark"
        echo "  $0 benchmark empty-program  # Run empty program benchmark"
        ;;
esac
