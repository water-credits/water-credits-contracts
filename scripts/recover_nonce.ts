import { Contract, rpc, nativeToScVal, scValToNative } from '@stellar/stellar-sdk';

/**
 * Recovers the expected next nonce for an oracle for a given project.
 * Useful when an oracle node loses its state and needs to resume submissions.
 * 
 * @param rpcUrl The Soroban RPC URL
 * @param oracleContractId The verification_oracle contract ID
 * @param projectId The 32-byte hex project ID
 * @param oracleAddress The oracle's stellar address
 * @returns The next expected nonce
 */
export async function recoverOracleNonce(
  rpcUrl: string,
  oracleContractId: string,
  projectId: string,
  oracleAddress: string
): Promise<number> {
  const server = new rpc.Server(rpcUrl);
  
  // Convert hex project ID to 32-byte Buffer
  const projectIdBuf = Buffer.from(projectId, 'hex');
  if (projectIdBuf.length !== 32) {
    throw new Error('Project ID must be exactly 32 bytes (64 hex characters)');
  }

  // Create contract instance
  const contract = new Contract(oracleContractId);
  
  // Prepare invocation
  const txBuilder = await server.prepareTransaction({
    source: oracleAddress, // or any address
    networkPassphrase: 'Test SDF Network ; September 2015',
    fee: '100',
    sequence: '0',
    operations: [
      contract.call('get_oracle_nonce',
        nativeToScVal(projectIdBuf, { type: 'bytes' }),
        nativeToScVal(oracleAddress, { type: 'address' })
      )
    ]
  });

  // We can simulate the transaction to read the state
  const simulation = await server.simulateTransaction(txBuilder as any);
  
  if (rpc.Api.isSimulationError(simulation)) {
    throw new Error(`Simulation failed: ${simulation.error}`);
  }

  if (rpc.Api.isSimulationSuccess(simulation)) {
    // The result is the first entry in results array
    const resultVal = simulation.result.retval;
    const nextNonce = scValToNative(resultVal);
    return Number(nextNonce);
  }
  
  throw new Error('Failed to simulate transaction');
}

// Example usage when run directly
if (require.main === module) {
  const rpcUrl = process.env.RPC_URL || 'https://soroban-testnet.stellar.org';
  const contractId = process.argv[2];
  const projectId = process.argv[3];
  const oracle = process.argv[4];

  if (!contractId || !projectId || !oracle) {
    console.error('Usage: npx ts-node recover_nonce.ts <contract_id> <project_id_hex> <oracle_address>');
    process.exit(1);
  }

  recoverOracleNonce(rpcUrl, contractId, projectId, oracle)
    .then(nonce => {
      console.log(`Expected next nonce for oracle ${oracle} on project ${projectId}: ${nonce}`);
      process.exit(0);
    })
    .catch(err => {
      console.error('Error recovering nonce:', err);
      process.exit(1);
    });
}
