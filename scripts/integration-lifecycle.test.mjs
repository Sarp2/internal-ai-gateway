import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createIntegrationLifecycle } from './integration-lifecycle.mjs';

test('runs deploy, tests, and destroy in order', async () => {
	const harness = createHarness();

	assert.equal(await harness.lifecycle.run(), 0);
	assert.deepEqual(harness.calls, ['deploy', 'test', 'destroy']);
});

test('destroys resources when deployment fails', async () => {
	const harness = createHarness({ deployExitCode: 1 });

	assert.equal(await harness.lifecycle.run(), 1);
	assert.deepEqual(harness.calls, ['deploy', 'destroy']);
});

test('destroys resources when integration tests fail', async () => {
	const harness = createHarness({ testExitCode: 1 });

	assert.equal(await harness.lifecycle.run(), 1);
	assert.deepEqual(harness.calls, ['deploy', 'test', 'destroy']);
});

test('destroys resources once when interrupted', async () => {
	let finishTests;

	const testOperation = new Promise((resolve) => {
		finishTests = resolve;
	});

	const harness = createHarness({ testOperation });
	const runPromise = harness.lifecycle.run();
	await waitForCall(harness.calls, 'test');

	const interruptionPromise = harness.lifecycle.handleInterruption('SIGINT');
	finishTests(1);
	await Promise.all([runPromise, interruptionPromise]);

	assert.deepEqual(harness.calls, ['deploy', 'test', 'stop:SIGINT', 'destroy', 'exit:130']);
});

test('does not destroy resources twice for repeated signals', async () => {
	let finishTests;
	const testOperation = new Promise((resolve) => {
		finishTests = resolve;
	});

	const harness = createHarness({ testOperation });
	const runPromise = harness.lifecycle.run();
	await waitForCall(harness.calls, 'test');

	const firstSignal = harness.lifecycle.handleInterruption('SIGTERM');
	const secondSignal = harness.lifecycle.handleInterruption('SIGINT');
	finishTests(1);
	await Promise.all([runPromise, firstSignal, secondSignal]);

	assert.deepEqual(harness.calls, ['deploy', 'test', 'stop:SIGTERM', 'destroy', 'exit:143']);
});

test('reports cleanup failure after interruption', async () => {
	let finishTests;
	const testOperation = new Promise((resolve) => {
		finishTests = resolve;
	});

	const harness = createHarness({ destroyExitCode: 1, testOperation });
	const runPromise = harness.lifecycle.run();
	await waitForCall(harness.calls, 'test');

	const interruptionPromise = harness.lifecycle.handleInterruption('SIGTERM');
	finishTests(1);
	await Promise.all([runPromise, interruptionPromise]);

	assert.deepEqual(harness.errors, [
		'Received SIGTERM; destroying integration resources.',
		'Integration resource cleanup failed after interruption.',
	]);
});

function createHarness({
	deployExitCode = 0,
	destroyExitCode = 0,
	testExitCode = 0,
	testOperation,
} = {}) {
	const calls = [];
	const errors = [];
	const lifecycle = createIntegrationLifecycle({
		deploy: async () => {
			calls.push('deploy');
			return deployExitCode;
		},
		destroy: async () => {
			calls.push('destroy');
			return destroyExitCode;
		},
		exit: (code) => calls.push(`exit:${code}`),
		logError: (message) => errors.push(message),
		runTests: async () => {
			calls.push('test');
			return testOperation === undefined ? testExitCode : testOperation;
		},
		stop: (signal) => calls.push(`stop:${signal}`),
	});

	return { calls, errors, lifecycle };
}

async function waitForCall(calls, expectedCall) {
	while (!calls.includes(expectedCall)) {
		await new Promise((resolve) => setImmediate(resolve));
	}
}
