#!/usr/bin/env bash
# setup.sh — Deploy factory Fargate infrastructure and write local config.
#
# Usage: ./infrastructure/setup.sh [--region REGION]
#
# Deploys the CloudFormation stack, builds and pushes the Docker image,
# and writes fargate.env for the factory command.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
FACTORY_CONFIG="${FACTORY_CONFIG:-$HOME/.config/factory}"

REGION="${AWS_DEFAULT_REGION:-us-west-1}"
STACK_NAME="factory"

# Parse args
while [ $# -gt 0 ]; do
  case "$1" in
    --region) REGION="$2"; shift 2 ;;
    *) echo "Unknown option: $1" >&2; exit 1 ;;
  esac
done

die() { printf 'setup: %s\n' "$1" >&2; exit 1; }

command -v aws >/dev/null 2>&1 || die "aws CLI not found"
command -v docker >/dev/null 2>&1 || die "docker not found"

# Set DOCKER_HOST for Podman if not already set
if [ -z "${DOCKER_HOST:-}" ] && command -v podman >/dev/null 2>&1; then
  DOCKER_HOST="$(podman machine inspect --format '{{.ConnectionInfo.PodmanSocket.Path}}' 2>/dev/null)" || true
  if [ -n "$DOCKER_HOST" ]; then
    export DOCKER_HOST="unix://${DOCKER_HOST}"
  fi
fi

# -------------------------------------------------------------------------
# Discover VPC and subnets
# -------------------------------------------------------------------------

printf 'Discovering default VPC in %s...\n' "$REGION"
VPC_ID="$(aws ec2 describe-vpcs \
  --region "$REGION" \
  --filters "Name=is-default,Values=true" \
  --query 'Vpcs[0].VpcId' \
  --output text)"

[ "$VPC_ID" != "None" ] && [ -n "$VPC_ID" ] || die "No default VPC found in $REGION"

SUBNET_IDS="$(aws ec2 describe-subnets \
  --region "$REGION" \
  --filters "Name=vpc-id,Values=${VPC_ID}" \
  --query 'Subnets[*].SubnetId' \
  --output text | tr '\t' ',')"

[ -n "$SUBNET_IDS" ] || die "No subnets found in VPC $VPC_ID"

printf '  VPC:     %s\n' "$VPC_ID"
printf '  Subnets: %s\n' "$SUBNET_IDS"

# -------------------------------------------------------------------------
# Deploy CloudFormation stack
# -------------------------------------------------------------------------

printf '\nDeploying CloudFormation stack "%s"...\n' "$STACK_NAME"
aws cloudformation deploy \
  --region "$REGION" \
  --stack-name "$STACK_NAME" \
  --template-file "${SCRIPT_DIR}/cloudformation.yaml" \
  --parameter-overrides \
    "VpcId=${VPC_ID}" \
    "SubnetIds=${SUBNET_IDS}" \
  --capabilities CAPABILITY_NAMED_IAM \
  --no-fail-on-empty-changeset

printf '  Stack deployed.\n'

# -------------------------------------------------------------------------
# Read stack outputs
# -------------------------------------------------------------------------

get_output() {
  aws cloudformation describe-stacks \
    --region "$REGION" \
    --stack-name "$STACK_NAME" \
    --query "Stacks[0].Outputs[?OutputKey=='${1}'].OutputValue" \
    --output text
}

CLUSTER_ARN="$(get_output ClusterArn)"
RUN_TASK_DEF="$(get_output RunTaskDefArn)"
REPO_URI="$(get_output RepoUri)"
S3_BUCKET="$(get_output WorkspaceBucketName)"
SG_ID="$(get_output TaskSecurityGroupId)"

printf '\n  Cluster:   %s\n' "$CLUSTER_ARN"
printf '  Task def:  %s\n' "$RUN_TASK_DEF"
printf '  S3 bucket: %s\n' "$S3_BUCKET"

# -------------------------------------------------------------------------
# Build and push Docker image
# -------------------------------------------------------------------------

ACCOUNT_ID="$(aws sts get-caller-identity --query Account --output text)"

printf '\nAuthenticating Docker with ECR...\n'
aws ecr get-login-password --region "$REGION" | \
  docker login --username AWS --password-stdin \
  "${ACCOUNT_ID}.dkr.ecr.${REGION}.amazonaws.com"

printf '\nBuilding run image...\n'
docker build \
  --platform linux/amd64 \
  --load \
  -f "${SCRIPT_DIR}/run/Dockerfile" \
  -t "${REPO_URI}:latest" \
  "${SCRIPT_DIR}/.."

printf '\nPushing image...\n'
docker push "${REPO_URI}:latest"

printf '  Image pushed.\n'

# -------------------------------------------------------------------------
# Write fargate.env config
# -------------------------------------------------------------------------

mkdir -p "$FACTORY_CONFIG"

cat > "${FACTORY_CONFIG}/fargate.env" <<EOF
FACTORY_CLUSTER=${CLUSTER_ARN}
FACTORY_RUN_TASK=${RUN_TASK_DEF}
FACTORY_S3_BUCKET=${S3_BUCKET}
FACTORY_SUBNETS=${SUBNET_IDS}
FACTORY_SECURITY_GROUP=${SG_ID}
FACTORY_REGION=${REGION}
EOF

printf '\n  Config written to %s/fargate.env\n' "$FACTORY_CONFIG"
printf '\nSetup complete. Run: factory run --runtime fargate\n'
