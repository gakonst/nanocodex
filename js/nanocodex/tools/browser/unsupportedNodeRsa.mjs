export default class UnsupportedNodeRsa {
    constructor() {
        throw new Error("node-rsa is unavailable in browsers; Web Crypto must be used");
    }
}
