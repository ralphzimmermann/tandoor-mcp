#!/usr/bin/env bash
set -e

# Color codes for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m' # No Color

# Configuration
COMPOSE_FILE="docker-compose.yml"
ENV_FILE=".env.dev"
WAIT_TIME=30
# A clean Tandoor image can spend several minutes applying migrations.
MAX_RETRIES="${MAX_RETRIES:-60}"
TANDOOR_TEST_PORT="${TANDOOR_TEST_PORT:-8080}"

# The Compose stack publishes Tandoor on the *Docker host*, which is only
# localhost when the active Docker context is local. For an ssh:// or tcp://
# context (a remote engine), derive the host from the context so the readiness
# probe, token fetch, and tests reach the published port. Override with
# TANDOOR_TEST_HOST when the engine is reachable under a different name.
detect_docker_host() {
    local endpoint host
    endpoint=$(docker context inspect --format '{{.Endpoints.docker.Host}}' 2>/dev/null || true)
    case "$endpoint" in
        ssh://*|tcp://*)
            host="${endpoint#*://}"   # strip scheme
            host="${host##*@}"        # strip user@
            host="${host%%/*}"        # strip any path
            host="${host%%:*}"        # strip port
            echo "${host:-localhost}"
            ;;
        *)
            echo "localhost"
            ;;
    esac
}
TANDOOR_TEST_HOST="${TANDOOR_TEST_HOST:-$(detect_docker_host)}"
TANDOOR_TEST_URL="http://${TANDOOR_TEST_HOST}:${TANDOOR_TEST_PORT}"

# Parse command line arguments
ACTION="${1:-test}"
KEEP_RUNNING=false
VERBOSE=false

while [[ $# -gt 0 ]]; do
    case $1 in
        --keep-running)
            KEEP_RUNNING=true
            shift
            ;;
        --verbose|-v)
            VERBOSE=true
            shift
            ;;
        clean)
            ACTION="clean"
            shift
            ;;
        test)
            ACTION="test"
            shift
            ;;
        up)
            ACTION="up"
            shift
            ;;
        down)
            ACTION="down"
            shift
            ;;
        reset)
            ACTION="reset"
            shift
            ;;
        logs)
            ACTION="logs"
            shift
            ;;
        *)
            shift
            ;;
    esac
done

# Functions
log_info() {
    echo -e "${BLUE}[INFO]${NC} $1"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $1"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $1"
}

log_warning() {
    echo -e "${YELLOW}[WARNING]${NC} $1"
}

clean_volumes() {
    log_info "Cleaning Docker volumes and data..."
    docker compose -f "$COMPOSE_FILE" down -v 2>/dev/null || true
    
    # Try to remove with current user first, then with sudo if needed
    if ! rm -rf ./postgresql ./staticfiles ./mediafiles 2>/dev/null; then
        log_warning "Need elevated permissions to clean volumes"
        if command -v sudo &> /dev/null; then
            sudo rm -rf ./postgresql ./staticfiles ./mediafiles
        else
            log_error "Cannot clean volumes without sudo"
            return 1
        fi
    fi
    
    log_success "Volumes cleaned"
}

start_services() {
    # The Tandoor image is the deployed fork on GHCR. Pull it explicitly so a
    # missing registry login fails with a hint instead of a bare compose error.
    log_info "Pulling the pinned Tandoor image..."
    if ! docker compose -f "$COMPOSE_FILE" pull --quiet web_recipes; then
        log_error "Could not pull the Tandoor image (see docker-compose.yml web_recipes)."
        log_error "If the GHCR package is private, log in first with a token that has read:packages:"
        log_error "  gh auth refresh -s read:packages && gh auth token | docker login ghcr.io -u <github-user> --password-stdin"
        return 1
    fi

    log_info "Starting Docker services..."
    docker compose -f "$COMPOSE_FILE" up -d
    
    log_info "Waiting for services to be ready at ${TANDOOR_TEST_URL}..."
    local retries=0
    while [ $retries -lt $MAX_RETRIES ]; do
        # Current Tandoor images do not include curl, so probe through the
        # published host port instead of executing a health check in-container.
        if curl -sf "${TANDOOR_TEST_URL}/login/" > /dev/null 2>&1; then
            log_success "Tandoor is ready!"
            return 0
        fi
        retries=$((retries + 1))
        log_info "Waiting for Tandoor... (attempt $retries/$MAX_RETRIES)"
        sleep 5
    done
    
    log_error "Tandoor failed to start within timeout"
    docker compose -f "$COMPOSE_FILE" logs web_recipes
    return 1
}

stop_services() {
    log_info "Stopping Docker services..."
    docker compose -f "$COMPOSE_FILE" down
    log_success "Services stopped"
}

create_admin_user() {
    log_info "Creating admin user and setting up permissions..."
    
    # Create superuser and set up proper space/permissions using Django management command
    docker compose -f "$COMPOSE_FILE" exec -T web_recipes /opt/recipes/venv/bin/python manage.py shell <<EOF
from django.contrib.auth import get_user_model
from django.contrib.auth.models import Group
from cookbook.models import Space, UserSpace

# Create or get admin user
User = get_user_model()
user, created = User.objects.get_or_create(
    username='admin',
    defaults={
        'email': 'admin@example.com',
        'is_superuser': True,
        'is_staff': True
    }
)
if created:
    user.set_password('testing1')
    user.save()
    print('Admin user created')
else:
    print('Admin user already exists')

# Create a default space if it doesn't exist
space, space_created = Space.objects.get_or_create(
    name='Default',
    defaults={'created_by': user}
)
if space_created:
    print('Default space created')

# Get or create user and admin groups
user_group, _ = Group.objects.get_or_create(name='user')
admin_group, _ = Group.objects.get_or_create(name='admin')

# Create UserSpace with admin permissions
user_space, us_created = UserSpace.objects.get_or_create(
    user=user,
    space=space,
    defaults={'active': True}
)
if us_created:
    print('UserSpace created')

# Add user to admin group for this space
user_space.groups.add(admin_group)
user_space.groups.add(user_group)

print(f"Admin user configured with access to space '{space.name}' and groups: {list(user_space.groups.values_list('name', flat=True))}")
EOF
    
    log_success "Admin user and permissions configured"
}

run_tests() {
    log_info "Running integration tests..."
    
    # Set environment variables for the MCP server
    export TANDOOR_BASE_URL="${TANDOOR_TEST_URL}"
    export TANDOOR_USERNAME="admin"
    export TANDOOR_PASSWORD="testing1"
    
    # Try to get an auth token to avoid rate limiting (10 requests/day!)
    log_info "Getting authentication token..."
    TOKEN=$(bash ./scripts/get_token.sh "$TANDOOR_USERNAME" "$TANDOOR_PASSWORD" "$TANDOOR_BASE_URL" 2>/dev/null)
    
    if [ -n "$TOKEN" ]; then
        export TANDOOR_AUTH_TOKEN="$TOKEN"
        log_success "Got auth token, tests will reuse it"
    else
        log_warning "Could not get auth token - tests may fail due to rate limiting"
        log_warning "Tandoor limits auth to 10 requests/day. Consider restarting Tandoor to reset."
    fi
    
    # Run Rust tests
    if [ "$VERBOSE" = true ]; then
        cargo test --test '*' -- --nocapture --test-threads=1
    else
        cargo test --test '*' -- --test-threads=1
    fi
    
    local test_result=$?
    
    if [ $test_result -eq 0 ]; then
        log_success "All tests passed!"
    else
        log_error "Tests failed!"
        return $test_result
    fi
}

show_logs() {
    docker compose -f "$COMPOSE_FILE" logs -f
}

# Main execution
main() {
    case "$ACTION" in
        clean)
            clean_volumes
            ;;
        up)
            start_services
            create_admin_user
            ;;
        down)
            stop_services
            ;;
        reset)
            clean_volumes
            start_services
            create_admin_user
            ;;
        logs)
            show_logs
            ;;
        test)
            # Full test cycle
            log_info "Starting full test cycle..."
            
            # Clean and start fresh
            clean_volumes
            start_services
            create_admin_user
            
            # Run tests
            run_tests
            test_result=$?
            
            # Clean up unless --keep-running flag is set
            if [ "$KEEP_RUNNING" = false ]; then
                stop_services
            else
                log_info "Services kept running. Use './scripts/test.sh down' to stop them."
            fi
            
            exit $test_result
            ;;
        *)
            log_error "Unknown action: $ACTION"
            echo "Usage: $0 [clean|up|down|reset|test|logs] [--keep-running] [--verbose]"
            exit 1
            ;;
    esac
}

# Run main function
main