export function createIntegrationLifecycle({ deploy, destroy, exit, logError, runTests, stop }) {
	let activeOperation;
	let cleanupPromise;
	let interruptionExitCode;

	async function execute(operation) {
		const operationPromise = operation();
		activeOperation = operationPromise;

		try {
			return await operationPromise;
		} finally {
			if (activeOperation === operationPromise) {
				activeOperation = undefined;
			}
		}
	}

	function destroyOnce() {
		cleanupPromise ??= execute(destroy);
		return cleanupPromise;
	}

	async function runCompleteLifecycle() {
		const deployExitCode = await execute(deploy);
		if (deployExitCode !== 0) {
			const destroyExitCode = await destroyOnce();
			return deployExitCode || destroyExitCode;
		}

		const testExitCode = await execute(runTests);
		const destroyExitCode = await destroyOnce();
		return testExitCode || destroyExitCode;
	}

	async function handleInterruption(signal) {
		if (interruptionExitCode !== undefined) {
			return;
		}

		interruptionExitCode = signal === 'SIGINT' ? 130 : 143;
		logError(`Received ${signal}; destroying integration resources.`);

		if (cleanupPromise === undefined) {
			const interruptedOperation = activeOperation;
			stop(signal);
			await interruptedOperation;
		}

		const destroyExitCode = await destroyOnce();
		if (destroyExitCode !== 0) {
			logError('Integration resource cleanup failed after interruption.');
		}
		exit(interruptionExitCode);
	}

	return {
		deploy: () => execute(deploy),
		destroy: destroyOnce,
		handleInterruption,
		run: runCompleteLifecycle,
	};
}
