# Deploying

- **Read when:** shipping a new version to production or rolling one back.

Run `scripts/deploy.sh` from a clean main. It builds the image, runs the
migrations and switches traffic over.

## Rolling back

Run the script again with the previous tag.
