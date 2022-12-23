# ipfs-proxy

IPFS proxy that restricts path-based gateways. (aka. ipfs gateway dlya bomzhey)

## How it works

We download list of CID's from web3.storage and then we use it to restrict access to IPFS gateway.
If CID is not in the list, we return 404.

Updates from web3.storage are fetched every 30 seconds.

## Setup

Required environment variables:

- `STORAGE_API_KEY` - API key for web3.storage

Optional environment variables:

- `HOST` - host to listen on (default: `0.0.0.0:3000`).
- `IPFS_GATEWAY` - IPFS gateway to use (default: `https://ipfs.io/ipfs`).

You can use binary from flake.
