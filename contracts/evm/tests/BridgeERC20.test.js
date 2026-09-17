import { expect } from "chai";
import hre from "hardhat";
const { ethers } = hre;

describe("BridgeERC20", function () {
  let BridgeERC20;
  let bridge;
  let owner, admin, governance, user, relayer1, relayer2, relayer3, nonRelayer;
  let relayers;
  const threshold = 2;

  beforeEach(async function () {
    [owner, admin, governance, user, relayer1, relayer2, relayer3, nonRelayer] = await ethers.getSigners();
    
    // Sort relayers by address ascending to simplify testing sorted signatures
    relayers = [relayer1, relayer2, relayer3].sort((a, b) => {
      return a.address.toLowerCase().localeCompare(b.address.toLowerCase());
    });

    BridgeERC20 = await ethers.getContractFactory("BridgeERC20");
    bridge = await BridgeERC20.deploy(
      "Test Credit Token",
      "TST",
      admin.address,
      governance.address,
      relayers.map(r => r.address),
      threshold
    );
  });

  describe("Deployment", function () {
    it("Should set the correct configuration", async function () {
      expect(await bridge.name()).to.equal("Test Credit Token");
      expect(await bridge.symbol()).to.equal("TST");
      expect(await bridge.decimals()).to.equal(7);
      expect(await bridge.admin()).to.equal(admin.address);
      expect(await bridge.governance()).to.equal(governance.address);
      expect(await bridge.threshold()).to.equal(threshold);
      expect(await bridge.isRelayer(relayers[0].address)).to.be.true;
      expect(await bridge.isRelayer(nonRelayer.address)).to.be.false;
    });
  });

  describe("ERC20 / Deposit", function () {
    it("Should allow deposit and burn tokens", async function () {
      const sourceChain = 1; // Stellar
      const destinationChain = 2; // EVM
      const nonce = 1n;
      const sender = ethers.zeroPadValue(ethers.toBeArray(0x1234), 32);
      const amount = 1000n;

      // Generate message hash
      const msgHash = ethers.solidityPackedKeccak256(
        ["address", "uint32", "uint32", "uint64", "bytes32", "address", "uint256"],
        [bridge.target, sourceChain, destinationChain, nonce, sender, user.address, amount]
      );

      // Sign using relayers 0 and 1
      const sigs = [];
      sigs.push(await relayers[0].signMessage(ethers.getBytes(msgHash)));
      sigs.push(await relayers[1].signMessage(ethers.getBytes(msgHash)));

      // Sort sigs by relayer address
      const pairedSigs = [
        { address: relayers[0].address, sig: sigs[0] },
        { address: relayers[1].address, sig: sigs[1] }
      ].sort((a, b) => a.address.toLowerCase().localeCompare(b.address.toLowerCase()));

      await bridge.withdraw(
        sourceChain,
        destinationChain,
        nonce,
        sender,
        user.address,
        amount,
        pairedSigs.map(p => p.sig)
      );

      expect(await bridge.balanceOf(user.address)).to.equal(amount);
      expect(await bridge.totalSupply()).to.equal(amount);

      // Now deposit
      const stellarRecipient = ethers.zeroPadValue(ethers.toBeArray(0x5678), 32);
      
      await expect(bridge.connect(user).deposit(400n, 0, stellarRecipient))
        .to.emit(bridge, "Deposit")
        .withArgs(user.address, 0, stellarRecipient, 400n, 0n);

      expect(await bridge.balanceOf(user.address)).to.equal(600n);
      expect(await bridge.totalSupply()).to.equal(600n);
    });
  });

  describe("Withdrawal & Signature Verification", function () {
    const sourceChain = 1;
    const destinationChain = 2;
    const nonce = 1n;
    const sender = ethers.zeroPadValue(ethers.toBeArray(0x1234), 32);
    const amount = 500n;
    let msgHash;

    beforeEach(async function () {
      msgHash = ethers.solidityPackedKeccak256(
        ["address", "uint32", "uint32", "uint64", "bytes32", "address", "uint256"],
        [bridge.target, sourceChain, destinationChain, nonce, sender, user.address, amount]
      );
    });

    it("Should succeed with valid sorted signatures", async function () {
      const sigs = [
        await relayers[0].signMessage(ethers.getBytes(msgHash)),
        await relayers[1].signMessage(ethers.getBytes(msgHash))
      ];

      const paired = [
        { address: relayers[0].address, sig: sigs[0] },
        { address: relayers[1].address, sig: sigs[1] }
      ].sort((a, b) => a.address.toLowerCase().localeCompare(b.address.toLowerCase()));

      await expect(bridge.withdraw(
        sourceChain,
        destinationChain,
        nonce,
        sender,
        user.address,
        amount,
        paired.map(p => p.sig)
      )).to.emit(bridge, "Withdraw").withArgs(sender, user.address, amount, nonce);

      expect(await bridge.balanceOf(user.address)).to.equal(amount);
    });

    it("Should fail if signatures are not sorted", async function () {
      const unsortedRelayers = [...relayers].sort((a, b) => {
        return b.address.toLowerCase().localeCompare(a.address.toLowerCase());
      });

      const sigs = [
        await unsortedRelayers[0].signMessage(ethers.getBytes(msgHash)),
        await unsortedRelayers[1].signMessage(ethers.getBytes(msgHash))
      ];

      await expect(bridge.withdraw(
        sourceChain,
        destinationChain,
        nonce,
        sender,
        user.address,
        amount,
        sigs
      )).to.be.revertedWith("Signatures must be sorted and unique");
    });

    it("Should fail if same signature is duplicated", async function () {
      const sig = await relayers[0].signMessage(ethers.getBytes(msgHash));
      await expect(bridge.withdraw(
        sourceChain,
        destinationChain,
        nonce,
        sender,
        user.address,
        amount,
        [sig, sig]
      )).to.be.revertedWith("Signatures must be sorted and unique");
    });

    it("Should fail with non-relayer signatures", async function () {
      const sigs = [
        await relayers[0].signMessage(ethers.getBytes(msgHash)),
        await nonRelayer.signMessage(ethers.getBytes(msgHash))
      ];

      const paired = [
        { address: relayers[0].address, sig: sigs[0] },
        { address: nonRelayer.address, sig: sigs[1] }
      ].sort((a, b) => a.address.toLowerCase().localeCompare(b.address.toLowerCase()));

      await expect(bridge.withdraw(
        sourceChain,
        destinationChain,
        nonce,
        sender,
        user.address,
        amount,
        paired.map(p => p.sig)
      )).to.be.revertedWith("Invalid relayer signature");
    });

    it("Should enforce replay protection", async function () {
      const sigs = [
        await relayers[0].signMessage(ethers.getBytes(msgHash)),
        await relayers[1].signMessage(ethers.getBytes(msgHash))
      ];
      const paired = [
        { address: relayers[0].address, sig: sigs[0] },
        { address: relayers[1].address, sig: sigs[1] }
      ].sort((a, b) => a.address.toLowerCase().localeCompare(b.address.toLowerCase()));

      await bridge.withdraw(
        sourceChain,
        destinationChain,
        nonce,
        sender,
        user.address,
        amount,
        paired.map(p => p.sig)
      );

      await expect(bridge.withdraw(
        sourceChain,
        destinationChain,
        nonce,
        sender,
        user.address,
        amount,
        paired.map(p => p.sig)
      )).to.be.revertedWith("Message already processed");
    });
  });

  describe("Governance and Pause Control", function () {
    it("Should allow admin or governance to pause and block operations", async function () {
      await expect(bridge.connect(user).pause()).to.be.revertedWith("Unauthorized");

      await expect(bridge.connect(admin).pause())
        .to.emit(bridge, "Paused")
        .withArgs(admin.address);

      expect(await bridge.paused()).to.be.true;

      await expect(bridge.connect(user).deposit(100n, 0, ethers.zeroPadValue(ethers.toBeArray(0x12), 32)))
        .to.be.revertedWith("Paused");

      await expect(bridge.connect(governance).unpause())
        .to.emit(bridge, "Unpaused")
        .withArgs(governance.address);

      expect(await bridge.paused()).to.be.false;
    });
  });
});
