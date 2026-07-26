import { test } from 'node:test';
import { App, Stack } from 'aws-cdk-lib';
import { Match, Template } from 'aws-cdk-lib/assertions';
import type { IVpc } from 'aws-cdk-lib/aws-ec2';
import { SecurityGroup } from 'aws-cdk-lib/aws-ec2';
import { Cluster } from 'aws-cdk-lib/aws-ecs';
import { PrivateDnsNamespace } from 'aws-cdk-lib/aws-servicediscovery';
import { NetworkStack } from './network-stack.ts';
import { ProviderMockStack } from './provider-mock-stack.ts';

test('runs both provider mocks as private Fargate services', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::ECS::Service', 2);
	template.resourceCountIs('AWS::ECS::TaskDefinition', 2);
	template.hasResourceProperties('AWS::ECS::Service', {
		DesiredCount: 1,
		EnableECSManagedTags: true,
		LaunchType: 'FARGATE',
		NetworkConfiguration: {
			AwsvpcConfiguration: {
				AssignPublicIp: 'DISABLED',
				Subnets: Match.anyValue(),
			},
		},
		ServiceRegistries: [
			{
				RegistryArn: Match.anyValue(),
			},
		],
	});
	template.hasResourceProperties('AWS::ECS::TaskDefinition', {
		ContainerDefinitions: [
			Match.objectLike({
				Environment: [
					{
						Name: 'PORT',
						Value: '8080',
					},
				],
				Essential: true,
				HealthCheck: Match.objectLike({
					Command: Match.arrayWith([
						'CMD-SHELL',
						Match.stringLikeRegexp('127\\.0\\.0\\.1:8080/health'),
					]),
				}),
				Name: 'anthropic-provider-mock',
				PortMappings: [
					Match.objectLike({
						ContainerPort: 8080,
					}),
				],
			}),
		],
		Cpu: '256',
		Memory: '512',
	});
	template.hasResourceProperties('AWS::ECS::TaskDefinition', {
		ContainerDefinitions: [
			Match.objectLike({
				Name: 'openai-provider-mock',
				PortMappings: [
					Match.objectLike({
						ContainerPort: 8080,
					}),
				],
			}),
		],
		Cpu: '256',
		Memory: '512',
	});
});

test('registers both provider mocks in private DNS', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::ServiceDiscovery::Service', 2);
	template.hasResourceProperties('AWS::ServiceDiscovery::Service', {
		DnsConfig: {
			DnsRecords: [
				{
					TTL: 10,
					Type: 'A',
				},
			],
			NamespaceId: Match.anyValue(),
			RoutingPolicy: 'MULTIVALUE',
		},
		Name: 'anthropic-provider-mock',
	});
	template.hasResourceProperties('AWS::ServiceDiscovery::Service', {
		DnsConfig: {
			DnsRecords: [
				{
					TTL: 10,
					Type: 'A',
				},
			],
			NamespaceId: Match.anyValue(),
			RoutingPolicy: 'MULTIVALUE',
		},
		Name: 'openai-provider-mock',
	});
});

test('allows only the integration proxy security group on each mock port', () => {
	const template = synthesizeTemplate();

	template.resourceCountIs('AWS::EC2::SecurityGroupIngress', 2);
	template.hasResourceProperties('AWS::EC2::SecurityGroupIngress', {
		Description: 'Allow Anthropic mock requests from the integration proxy.',
		FromPort: 8080,
		IpProtocol: 'tcp',
		SourceSecurityGroupId: {
			'Fn::ImportValue': Match.stringLikeRegexp(
				'TestIntegrationResourcesStack:ExportsOutputFnGetAttTestProxySecurityGroup.*GroupId',
			),
		},
		ToPort: 8080,
	});
	template.hasResourceProperties('AWS::EC2::SecurityGroupIngress', {
		Description: 'Allow OpenAI mock requests from the integration proxy.',
		FromPort: 8080,
		IpProtocol: 'tcp',
		SourceSecurityGroupId: {
			'Fn::ImportValue': Match.stringLikeRegexp(
				'TestIntegrationResourcesStack:ExportsOutputFnGetAttTestProxySecurityGroup.*GroupId',
			),
		},
		ToPort: 8080,
	});
});

function synthesizeTemplate(): Template {
	const app = new App();
	const networkStack = new NetworkStack(app, 'TestIntegrationNetworkStack');
	const resourcesStack = new Stack(app, 'TestIntegrationResourcesStack');
	const cluster = new Cluster(resourcesStack, 'TestCluster', {
		vpc: networkStack.vpc as IVpc,
	});
	const namespace = new PrivateDnsNamespace(resourcesStack, 'TestNamespace', {
		name: 'integration.internal',
		vpc: networkStack.vpc as IVpc,
	});
	const proxySecurityGroup = new SecurityGroup(resourcesStack, 'TestProxySecurityGroup', {
		vpc: networkStack.vpc as IVpc,
	});
	const stack = new ProviderMockStack(app, 'TestProviderMockStack', {
		cluster,
		namespace,
		proxySecurityGroup,
		vpc: networkStack.vpc,
	});

	return Template.fromStack(stack);
}
