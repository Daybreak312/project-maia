#!/bin/bash
set -e

# Maia Local Development Script
# Starts all services for local development

echo "Starting Maia development environment..."

# Start Qdrant if not running
if ! docker ps | grep -q qdrant; then
    echo "Starting Qdrant..."
    docker compose -f docker-compose.dev.yml up qdrant -d
    sleep 3
fi

# Check Qdrant health
echo "Checking Qdrant..."
curl -s http://localhost:6333/health || {
    echo "Qdrant is not responding. Please check docker logs."
    exit 1
}

# Build backend
echo "Building backend..."
cargo build

# Start backend in background
echo "Starting backend..."
DATA_DIR=./data cargo run &
BACKEND_PID=$!

# Wait for backend to start
sleep 3
curl -s http://localhost:8080/health > /dev/null || {
    echo "Backend failed to start"
    kill $BACKEND_PID 2>/dev/null
    exit 1
}

# Start frontend dev server
echo "Starting frontend..."
cd frontend && npm run dev &
FRONTEND_PID=$!

echo ""
echo "Development environment started!"
echo "  - Backend:  http://localhost:8080"
echo "  - Frontend: http://localhost:5173"
echo ""
echo "Press Ctrl+C to stop all services"

# Wait for Ctrl+C
trap "kill $BACKEND_PID $FRONTEND_PID 2>/dev/null; exit" INT
wait
