import { test } from 'node:test';
import { App } from 'aws-cdk-lib';
import { Template } from 'aws-cdk-lib/assertions';
import { EcsStack } from './ecs-stack.ts';
import { NetworkStack } from './network-stack.ts';

test('defines an ECS cluster for proxy workloads', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::ECS::Cluster', 1);
	template.hasResourceProperties('AWS::ECS::Cluster', {
		ClusterName: 'internal-ai-gateway-proxy',
	});
});

test('enables Fargate capacity providers for the cluster', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::ClusterCapacityProviderAssociations', {
		CapacityProviders: ['FARGATE', 'FARGATE_SPOT'],
	});
});

test('enables enhanced container insights for observability', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::Cluster', {
		ClusterSettings: [
			{
				Name: 'containerInsights',
				Value: 'enhanced',
			},
		],
	});
});

function synthesizeTemplate(): Template {
	const app = new App();
	const networkStack = new NetworkStack(app, 'TestNetworkStack');
	const ecsStack = new EcsStack(app, 'TestEcsStack', {
		vpc: networkStack.vpc,
	});

	return Template.fromStack(ecsStack);
}
