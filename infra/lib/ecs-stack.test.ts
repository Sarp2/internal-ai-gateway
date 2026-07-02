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

test('defines a Fargate task definition for the proxy container', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::TaskDefinition', {
		Family: 'internal-ai-gateway-proxy',
		Cpu: '512',
		Memory: '1024',
		NetworkMode: 'awsvpc',
		RequiresCompatibilities: ['FARGATE'],
	});
});

test('defines the proxy container port and runtime environment', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::TaskDefinition', {
		ContainerDefinitions: [
			{
				Name: 'proxy',
				Essential: true,
				Environment: [
					{
						Name: 'NODE_ENV',
						Value: 'production',
					},
					{
						Name: 'PORT',
						Value: '8080',
					},
				],
				PortMappings: [
					{
						ContainerPort: 8080,
					},
				],
			},
		],
	});
});

test('defines CloudWatch logs for the proxy container', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::Logs::LogGroup', {
		LogGroupName: '/internal-ai-gateway/proxy',
		RetentionInDays: 30,
	});
});

test('defines a private Fargate service for proxy tasks', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ECS::Service', {
		ServiceName: 'internal-ai-gateway-proxy',
		DesiredCount: 2,
		EnableECSManagedTags: true,
		DeploymentConfiguration: {
			DeploymentCircuitBreaker: {
				Enable: true,
				Rollback: true,
			},
			MaximumPercent: 200,
			MinimumHealthyPercent: 100,
		},
		NetworkConfiguration: {
			AwsvpcConfiguration: {
				AssignPublicIp: 'DISABLED',
			},
		},
	});
});

test('defines autoscaling limits for proxy tasks', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ApplicationAutoScaling::ScalableTarget', {
		MaxCapacity: 10,
		MinCapacity: 2,
		ScalableDimension: 'ecs:service:DesiredCount',
		ServiceNamespace: 'ecs',
	});
});

test('defines CPU and memory target tracking scaling policies', () => {
	const template = synthesizeTemplate();

	template.hasResourceProperties('AWS::ApplicationAutoScaling::ScalingPolicy', {
		PolicyType: 'TargetTrackingScaling',
		TargetTrackingScalingPolicyConfiguration: {
			PredefinedMetricSpecification: {
				PredefinedMetricType: 'ECSServiceAverageCPUUtilization',
			},
			TargetValue: 60,
		},
	});

	template.hasResourceProperties('AWS::ApplicationAutoScaling::ScalingPolicy', {
		PolicyType: 'TargetTrackingScaling',
		TargetTrackingScalingPolicyConfiguration: {
			PredefinedMetricSpecification: {
				PredefinedMetricType: 'ECSServiceAverageMemoryUtilization',
			},
			TargetValue: 60,
		},
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
