# ipfs-proxy

IPFS proxy that restricts path-based gateways. (aka. ipfs gateway dlya bomzhey)

Repository contains only shitcode, so please, don't look at it.

## How it works

We download list of CID's from web3.storage and then we use it to restrict access to IPFS gateway.
If CID is not in the list, we return 404.

If CID is in the list, we return content from IPFS gateway.

All IPFS gateways are checked in parallel and first valid response is returned.

Updates from web3.storage are fetched every 2 seconds.

## Setup

Required environment variables:

- `STORAGE_API_KEY` - API key for web3.storage

Optional environment variables:

- `HOST` - host to listen on (default: `0.0.0.0:3000`).
- `IPFS_GATEWAYS` - IPFS gateways to use (default: `https://ipfs.io/ipfs`, `https://w3s.link/ipfs`, `https://cloudflare-ipfs.com/ipfs`, `https://hardbin.com/ipfs,https://gateway.pinata.cloud/ipfs`).

You can use binary from flake.
