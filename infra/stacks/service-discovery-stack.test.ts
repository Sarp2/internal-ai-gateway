import assert from 'node:assert/strict';
import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import { NetworkStack } from './network-stack.ts';
import { ServiceDiscoveryStack } from './service-discovery-stack.ts';

test('defines a private integration service-discovery namespace', () => {
	const { stack, template } = synthesizeTemplate();

	template.hasResourceProperties('AWS::ServiceDiscovery::PrivateDnsNamespace', {
		Description: 'Private service discovery namespace for integration-test services.',
		Name: 'integration.internal',
		Vpc: Match.anyValue(),
	});
	assert.equal(stack.namespace.namespaceName, 'integration.internal');
});

test('attaches the namespace to the integration VPC', () => {
	const { template } = synthesizeTemplate();

	template.hasResourceProperties('AWS::ServiceDiscovery::PrivateDnsNamespace', {
		Vpc: {
			'Fn::ImportValue': Match.stringLikeRegexp(
				'TestIntegrationNetworkStack:ExportsOutputRefGatewayVpc',
			),
		},
	});
});

function synthesizeTemplate(): {
	stack: ServiceDiscoveryStack;
	template: Template;
} {
	const app = new App();
	const networkStack = new NetworkStack(app, 'TestIntegrationNetworkStack');
	const stack = new ServiceDiscoveryStack(app, 'TestServiceDiscoveryStack', {
		vpc: networkStack.vpc,
	});

	return { stack, template: Template.fromStack(stack) };
}
