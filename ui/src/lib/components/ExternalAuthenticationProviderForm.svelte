<script lang="ts">
	import { create } from '@bufbuild/protobuf';
	import {
		AccessRole,
		CameraAccessPolicySchema,
		ExternalRoleMappingSchema,
		type ExternalAuthenticationProvider
	} from '$lib/proto/webrtc_pb';
	import { newExternalProvider, textList } from '$lib/external-authentication-admin';
	import { Input } from './ui/input/index.js';
	import { Button } from './ui/button/index.js';
	let {
		provider = $bindable(),
		index,
		onremove
	}: { provider: ExternalAuthenticationProvider; index: number; onremove: () => void } = $props();
	function method(kind: string): void {
		provider.method = newExternalProvider(kind === 'proxy' ? 'proxy' : 'oidc').method;
	}
	function role(index: number, value: string): void {
		const mapping = provider.mappings[index];
		mapping.role = value === 'administrator' ? AccessRole.ADMINISTRATOR : AccessRole.USER;
		mapping.cameraAccess =
			mapping.role === AccessRole.USER ? create(CameraAccessPolicySchema) : undefined;
	}
</script>

<fieldset class="space-y-4 rounded-sm border border-hairline p-4">
	<legend class="px-1 text-sm font-semibold">Provider {index + 1}</legend>
	<div class="grid gap-3 sm:grid-cols-2">
		<label class="space-y-1 text-xs"
			>Provider ID<Input bind:value={provider.providerId} maxlength={64} required /></label
		>
		<label class="space-y-1 text-xs"
			>Display name<Input bind:value={provider.name} maxlength={64} required /></label
		>
		<label class="space-y-1 text-xs"
			>Method<select
				class="h-9 w-full rounded-sm border border-input bg-background px-3"
				value={provider.method.case}
				onchange={(event) => method(event.currentTarget.value)}
				><option value="oidc">OpenID Connect</option><option value="proxy"
					>Trusted identity proxy</option
				></select
			></label
		>
	</div>
	{#if provider.method.case === 'oidc'}
		{@const oidc = provider.method.value}
		<div class="grid gap-3 sm:grid-cols-2">
			<label class="space-y-1 text-xs"
				>Issuer URL<Input type="url" bind:value={oidc.issuer} required /></label
			>
			<label class="space-y-1 text-xs"
				>Client ID<Input bind:value={oidc.clientId} maxlength={256} required /></label
			>
			<label class="space-y-1 text-xs"
				>Client secret reference<Input
					value={oidc.clientSecretReference ?? ''}
					oninput={(event) => (oidc.clientSecretReference = event.currentTarget.value || undefined)}
					placeholder={'{secret:OIDC_CLIENT}'}
					autocomplete="off"
				/></label
			>
			<label class="space-y-1 text-xs"
				>Redirect URI<Input
					type="url"
					bind:value={oidc.redirectUri}
					placeholder="https://recorder.example/auth/callback"
					required
				/></label
			>
			<label class="space-y-1 text-xs"
				>Scopes (comma separated)<Input
					value={oidc.scopes.join(', ')}
					oninput={(event) => {
						oidc.scopes = textList(event.currentTarget.value);
					}}
				/></label
			>
			<label class="space-y-1 text-xs"
				>Display name claim<Input
					bind:value={oidc.displayNameClaim}
					maxlength={64}
					required
				/></label
			>
			<label class="space-y-1 text-xs"
				>Additional endpoint origins (comma separated)<Input
					value={oidc.endpointOrigins.join(', ')}
					oninput={(event) => {
						oidc.endpointOrigins = textList(event.currentTarget.value);
					}}
				/></label
			>
			<label class="space-y-1 text-xs"
				>Private issuer networks (CIDRs)<Input
					value={oidc.privateNetworks.join(', ')}
					oninput={(event) => {
						oidc.privateNetworks = textList(event.currentTarget.value);
					}}
				/></label
			>
			<label class="space-y-1 text-xs"
				>Provider logout URL (optional)<Input
					type="url"
					value={oidc.logoutUri ?? ''}
					oninput={(event) => (oidc.logoutUri = event.currentTarget.value || undefined)}
				/></label
			>
		</div>
	{:else if provider.method.case === 'proxy'}
		{@const proxy = provider.method.value}
		<div class="grid gap-3 sm:grid-cols-2">
			<label class="space-y-1 text-xs"
				>Trusted immediate peers (CIDRs)<Input
					value={proxy.trustedPeers.join(', ')}
					oninput={(event) => {
						proxy.trustedPeers = textList(event.currentTarget.value);
					}}
					required
				/></label
			>
			<label class="space-y-1 text-xs"
				>Subject header<Input bind:value={proxy.subjectHeader} maxlength={64} required /></label
			>
			<label class="space-y-1 text-xs"
				>Role header<Input bind:value={proxy.roleHeader} maxlength={64} required /></label
			>
			<label class="space-y-1 text-xs"
				>Name header (optional)<Input
					value={proxy.nameHeader ?? ''}
					oninput={(event) => (proxy.nameHeader = event.currentTarget.value || undefined)}
					maxlength={64}
				/></label
			>
			<label class="space-y-1 text-xs"
				>Shared secret header (optional)<Input
					value={proxy.secretHeader ?? ''}
					oninput={(event) => (proxy.secretHeader = event.currentTarget.value || undefined)}
					maxlength={64}
				/></label
			>
			<label class="space-y-1 text-xs"
				>Shared secret reference<Input
					value={proxy.sharedSecretReference ?? ''}
					oninput={(event) =>
						(proxy.sharedSecretReference = event.currentTarget.value || undefined)}
					placeholder={'{secret:PROXY_SECRET}'}
					autocomplete="off"
				/></label
			>
		</div>
	{/if}
	<p class="text-xs text-text-muted">
		Enter complete secret references only. Store secret values in the existing secrets.toml on the
		recorder.
	</p>
	{#each provider.mappings as mapping, mappingIndex (mappingIndex)}
		<fieldset class="space-y-3 border border-hairline p-3">
			<legend class="px-1 text-xs font-semibold">Role mapping {mappingIndex + 1}</legend>
			<div class="grid gap-3 sm:grid-cols-3">
				<label class="space-y-1 text-xs"
					>Claim<Input bind:value={mapping.claim} maxlength={64} required /></label
				>
				<label class="space-y-1 text-xs"
					>Exact claim value<Input bind:value={mapping.value} maxlength={256} required /></label
				>
				<label class="space-y-1 text-xs"
					>Granted role<select
						class="h-9 w-full rounded-sm border border-input bg-background px-3"
						value={mapping.role === AccessRole.ADMINISTRATOR ? 'administrator' : 'user'}
						onchange={(event) => role(mappingIndex, event.currentTarget.value)}
						><option value="user">User</option><option value="administrator">Administrator</option
						></select
					></label
				>
			</div>
			{#if mapping.role === AccessRole.USER && mapping.cameraAccess}
				<label class="flex items-center gap-2 text-xs"
					><input type="checkbox" bind:checked={mapping.cameraAccess.allCameras} />All cameras</label
				>
				{#if !mapping.cameraAccess.allCameras}
					<label class="block space-y-1 text-xs"
						>Camera IDs (comma separated)<Input
							value={mapping.cameraAccess.cameraIds.join(', ')}
							oninput={(event) => {
								if (mapping.cameraAccess)
									mapping.cameraAccess.cameraIds = textList(event.currentTarget.value);
							}}
						/></label
					>
					<label class="block space-y-1 text-xs"
						>Group IDs (comma separated)<Input
							value={mapping.cameraAccess.groupIds.join(', ')}
							oninput={(event) => {
								if (mapping.cameraAccess)
									mapping.cameraAccess.groupIds = textList(event.currentTarget.value);
							}}
						/></label
					>
					<p class="text-xs text-text-muted">An empty policy grants no cameras.</p>
				{/if}
			{/if}
			<Button
				type="button"
				variant="outline"
				size="sm"
				onclick={() => {
					provider.mappings = provider.mappings.filter((_, i) => i !== mappingIndex);
				}}>Remove mapping {mappingIndex + 1}</Button
			>
		</fieldset>
	{/each}
	<div class="flex flex-wrap gap-2">
		<Button
			type="button"
			variant="outline"
			disabled={provider.mappings.length >= 128}
			onclick={() =>
				provider.mappings.push(
					create(ExternalRoleMappingSchema, {
						claim: 'role',
						role: AccessRole.USER,
						cameraAccess: create(CameraAccessPolicySchema)
					})
				)}>Add role mapping</Button
		>
		<Button type="button" variant="outline" onclick={onremove}>Remove provider {index + 1}</Button>
	</div>
</fieldset>
