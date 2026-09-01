// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

contract BridgeERC20 {
    string public name;
    string public symbol;
    uint8 public immutable decimals = 7; // Matches Stellar credit_token
    uint256 public totalSupply;

    mapping(address => uint256) public balanceOf;
    mapping(address => mapping(address => uint256)) public allowance;

    address public admin;
    address public governance;
    bool public paused;

    // Relayer signature threshold verification
    uint256 public threshold;
    mapping(address => bool) public isRelayer;
    address[] public relayers;

    // Replay protection
    mapping(bytes32 => bool) public processedMessages;
    uint256 public depositNonce;

    event Transfer(address indexed from, address indexed to, uint256 value);
    event Approval(address indexed owner, address indexed spender, uint256 value);
    
    event Deposit(
        address indexed sender,
        uint8 recipientType,
        bytes32 indexed recipient,
        uint256 amount,
        uint256 nonce
    );
    event Withdraw(
        bytes32 indexed sender,
        address indexed recipient,
        uint256 amount,
        uint256 nonce
    );
    event Paused(address account);
    event Unpaused(address account);

    modifier onlyAdmin() {
        require(msg.sender == admin, "Only admin");
        _;
    }

    modifier onlyGovernance() {
        require(msg.sender == governance, "Only governance");
        _;
    }

    modifier whenNotPaused() {
        require(!paused, "Paused");
        _;
    }

    constructor(
        string memory _name,
        string memory _symbol,
        address _admin,
        address _governance,
        address[] memory _relayers,
        uint256 _threshold
    ) {
        name = _name;
        symbol = _symbol;
        admin = _admin;
        governance = _governance;
        threshold = _threshold;
        
        require(_threshold > 0 && _threshold <= _relayers.length, "Invalid threshold");
        for (uint256 i = 0; i < _relayers.length; i++) {
            address relayer = _relayers[i];
            require(relayer != address(0), "Invalid relayer");
            require(!isRelayer[relayer], "Duplicate relayer");
            isRelayer[relayer] = true;
            relayers.push(relayer);
        }
    }

    // ── ERC20 Functions ──

    function transfer(address to, uint256 amount) external whenNotPaused returns (bool) {
        require(to != address(0), "Invalid address");
        require(balanceOf[msg.sender] >= amount, "Insufficient balance");
        
        balanceOf[msg.sender] -= amount;
        balanceOf[to] += amount;
        
        emit Transfer(msg.sender, to, amount);
        return true;
    }

    function approve(address spender, uint256 amount) external whenNotPaused returns (bool) {
        allowance[msg.sender][spender] = amount;
        emit Approval(msg.sender, spender, amount);
        return true;
    }

    function transferFrom(address from, address to, uint256 amount) external whenNotPaused returns (bool) {
        require(to != address(0), "Invalid address");
        require(balanceOf[from] >= amount, "Insufficient balance");
        require(allowance[from][msg.sender] >= amount, "Insufficient allowance");
        
        allowance[from][msg.sender] -= amount;
        balanceOf[from] -= amount;
        balanceOf[to] += amount;
        
        emit Transfer(from, to, amount);
        return true;
    }

    // ── Pause control ──

    function pause() external {
        // Either governance or admin can pause
        require(msg.sender == governance || msg.sender == admin, "Unauthorized");
        paused = true;
        emit Paused(msg.sender);
    }

    function unpause() external {
        // Either governance or admin can unpause
        require(msg.sender == governance || msg.sender == admin, "Unauthorized");
        paused = false;
        emit Unpaused(msg.sender);
    }

    // ── Bridge Operations ──

    function deposit(uint256 amount, uint8 recipientType, bytes32 recipient) external whenNotPaused {
        require(amount > 0, "Amount must be positive");
        require(balanceOf[msg.sender] >= amount, "Insufficient balance");
        require(recipientType == 0 || recipientType == 1, "Invalid recipient type");
        require(recipient != bytes32(0), "Invalid recipient");

        balanceOf[msg.sender] -= amount;
        totalSupply -= amount;
        
        emit Transfer(msg.sender, address(0), amount);
        
        emit Deposit(msg.sender, recipientType, recipient, amount, depositNonce);
        depositNonce++;
    }

    function withdraw(
        uint32 sourceChain,
        uint32 destinationChain,
        uint64 nonce,
        bytes32 sender,
        address recipient,
        uint256 amount,
        bytes[] calldata signatures
    ) external whenNotPaused {
        require(destinationChain == 2, "Invalid destination chain"); // 2 = EVM
        require(amount > 0, "Amount must be positive");
        require(recipient != address(0), "Invalid recipient");

        // Hash message according to structured layout including address(this) to prevent cross-contract replay
        bytes32 msgHash = keccak256(abi.encodePacked(
            address(this),
            sourceChain,
            destinationChain,
            nonce,
            sender,
            recipient,
            amount
        ));

        // Replay protection
        require(!processedMessages[msgHash], "Message already processed");
        processedMessages[msgHash] = true;

        // Verify signatures
        bytes32 ethSignedMsgHash = keccak256(abi.encodePacked("\x19Ethereum Signed Message:\n32", msgHash));
        
        require(signatures.length >= threshold, "Insufficient signatures");
        
        address lastSigner = address(0);
        uint256 validSigs = 0;

        for (uint256 i = 0; i < signatures.length; i++) {
            address signer = recoverSigner(ethSignedMsgHash, signatures[i]);
            require(isRelayer[signer], "Invalid relayer signature");
            require(signer > lastSigner, "Signatures must be sorted and unique");
            lastSigner = signer;
            validSigs++;
        }

        require(validSigs >= threshold, "Threshold not met");

        // Mint tokens to recipient
        balanceOf[recipient] += amount;
        totalSupply += amount;

        emit Transfer(address(0), recipient, amount);
        emit Withdraw(sender, recipient, amount, nonce);
    }

    function recoverSigner(bytes32 ethSignedMsgHash, bytes memory sig) public pure returns (address) {
        require(sig.length == 65, "Invalid signature length");
        bytes32 r;
        bytes32 s;
        uint8 v;
        assembly {
            r := mload(add(sig, 32))
            s := mload(add(sig, 64))
            v := byte(0, mload(add(sig, 96)))
        }
        return ecrecover(ethSignedMsgHash, v, r, s);
    }

    // Deterministic and lossless mapping from EVM address to bytes32 (padded with 12 leading zeros)
    function addressToBytes32(address addr) public pure returns (bytes32) {
        return bytes32(uint256(uint160(addr)));
    }

    // Deterministic and lossless mapping from bytes32 to EVM address (truncating first 12 bytes)
    function bytes32ToAddress(bytes32 b) public pure returns (address) {
        return address(uint160(uint256(b)));
    }
}
