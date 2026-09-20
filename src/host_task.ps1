$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = [System.Text.UTF8Encoding]::new($false)
try {
    $r = $env:ARTERM_TASK_REQUEST | ConvertFrom-Json
    $v = $r.value
    $sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
    if ($r.op -eq 'identity') { ConvertTo-Json -Compress $sid; exit 0 }
    if ([System.Diagnostics.Process]::GetCurrentProcess().SessionId -eq 0) { throw 'Interactive user session required; Session 0 is not supported' }
    $service = New-Object -ComObject 'Schedule.Service'
    $service.Connect()
    $folder = $service.GetFolder('\')
    $all = @($folder.GetTasks(1))
    $launcher = Join-Path $env:SystemRoot 'System32\wscript.exe'
    function Quote-Arg($value) {
        return '"' + $value + ('\' * ($value.Length - $value.TrimEnd('\').Length)) + '"'
    }
    function Bootstrap-Path($exe) { return Join-Path ([IO.Path]::GetDirectoryName($exe)) 'arterm-host-check.vbs' }
    function Check-Arguments($exe, $root) {
        return '//B //Nologo //E:VBScript ' + (Quote-Arg (Bootstrap-Path $exe)) + ' ' + (Quote-Arg $exe) + ' ' + (Quote-Arg $root)
    }
    function Principal-Sid($principal) {
        # Names are translated only when verifying Scheduler readback, never to choose a user.
        if ($principal -match '^S-\d(-\d+)+$') {
            return ([System.Security.Principal.SecurityIdentifier]::new($principal)).Value
        }
        return ([System.Security.Principal.NTAccount]::new($principal)).Translate([System.Security.Principal.SecurityIdentifier]).Value
    }
    function Assert-Owned($task, $expected, $arguments, $source, $allowLegacy = $true) {
        $d = $task.Definition
        $actionArgs = $d.Actions.Item(1).Arguments
        $rootArg = Quote-Arg $expected.root
        $legacy = $allowLegacy -and $d.Actions.Item(1).Path -ieq (Quote-Arg $expected.exe) -and
            ($actionArgs -ceq ('run --data-root ' + $rootArg) -or $actionArgs -ceq ('supervise --data-root ' + $rootArg))
        $modern = $d.Actions.Item(1).Path -ieq (Quote-Arg $launcher) -and
            $expected.launcher -ieq $launcher -and $actionArgs -ceq (Check-Arguments $expected.exe $expected.root)
        if ($d.RegistrationInfo.Source -cne $source -or
            $d.RegistrationInfo.Description -cne $expected.root -or
            (Principal-Sid $d.Principal.UserId) -ne $sid -or $expected.sid -ne $sid -or
            $d.Principal.LogonType -ne 3 -or $d.Principal.RunLevel -ne 0 -or
            $d.Triggers.Count -ne 1 -or $d.Triggers.Item(1).Type -ne 9 -or
            (Principal-Sid $d.Triggers.Item(1).UserId) -ne $sid -or
            -not $d.Triggers.Item(1).Enabled -or
            $d.Actions.Count -ne 1 -or $d.Actions.Item(1).Type -ne 0 -or
            (-not $legacy -and -not $modern)) {
            throw "Refusing foreign or modified task: $($task.Name)"
        }
        if ($modern) {
            $repeat = $d.Triggers.Item(1).Repetition
            if ($repeat.Interval -cne 'PT10M' -or $repeat.Duration -or $repeat.StopAtDurationEnd) {
                throw "Owned task must repeat every PT10M indefinitely: $($task.Name)"
            }
            $sha = [Security.Cryptography.SHA256]::Create()
            try { $hash = [BitConverter]::ToString($sha.ComputeHash([IO.File]::ReadAllBytes((Bootstrap-Path $expected.exe)))).Replace('-', '') }
            finally { $sha.Dispose() }
            if ($hash -ine '__BOOTSTRAP_SHA256__') { throw "Refusing modified/unowned hidden bootstrap: $($task.Name)" }
        }
    }
    if ($r.op -eq 'list') {
        $found = @()
        foreach ($task in $all) {
            $d = $task.Definition
            if ($d.RegistrationInfo.Source -cne $v.source) { continue }
            if ((Principal-Sid $d.Principal.UserId) -ne $sid) { continue }
            if ($d.Actions.Count -ne 1) { throw "Malformed owned task: $($task.Name)" }
            $root = $d.RegistrationInfo.Description
            $exe = $null
            foreach ($candidate in $v.paths) {
                if ($d.Actions.Item(1).Path -ieq (Quote-Arg $candidate) -or
                    ($d.Actions.Item(1).Path -ieq (Quote-Arg $launcher) -and
                     $d.Actions.Item(1).Arguments -ceq (Check-Arguments $candidate $root))) { $exe = $candidate; break }
            }
            if (-not $exe) { continue }
            $expected = @{name=$task.Name; sid=$sid; exe=$exe; launcher=$launcher; root=$root; enabled=[bool]$task.Enabled; running=($task.State -eq 4); snapshot_xml=$task.Xml}
            Assert-Owned $task $expected (Check-Arguments $exe $root) $v.source
            $found += $expected
        }
        ConvertTo-Json -InputObject @($found) -Compress
        exit 0
    }
    $expected = $v.task
    if ($expected.sid -cne $sid) {
        throw 'Task identity does not match the current process token SID; no account-name or local-account fallback is permitted'
    }
    $task = $all | Where-Object { $_.Name -ieq $expected.name }
    if ($task) { Assert-Owned $task $expected $v.arguments $v.source }
    if ($r.op -eq 'check') {
        'null'
        exit 0
    }
    if ($r.op -eq 'state' -and -not $task) {
        '{"running":false,"enabled":false,"missing":true}'
        exit 0
    }
    if ($r.op -eq 'remove-if-owned' -and -not $task) { 'null'; exit 0 }
    if ($r.op -eq 'register' -or $r.op -eq 'restore') {
        $proposed = $service.NewTask(0)
        $restore = $r.op -eq 'restore'
        if ($restore -and $expected.snapshot_xml) { $proposed.XmlText = $expected.snapshot_xml }
        else { $proposed.XmlText = $v.xml }
        Assert-Owned ([pscustomobject]@{Definition=$proposed; Name=$expected.name}) $expected $v.arguments $v.source $restore
        # Preserve the actual paused state, not the earlier snapshot's enabled flag.
        $proposed.Settings.Enabled = $false
        if ($task -and -not $restore) { $proposed.Settings.Enabled = $task.Enabled }
        $flags = 2
        if ($task) { $flags = 4 }
        $null = $folder.RegisterTask($expected.name, $proposed.XmlText, $flags, $sid, $null, 3, $null)
        # Reopen persisted state: Scheduler may normalize SID text to a name.
        $task = $folder.GetTask($expected.name)
        Assert-Owned $task $expected $v.arguments $v.source $restore
        if ([bool]$task.Enabled -ne [bool]$proposed.Settings.Enabled) { throw 'Persisted task enabled state differs from requested state' }
    } else {
        if (-not $task) { throw "Owned task is missing: $($expected.name)" }
        switch ($r.op) {
            'state' {
                @{ running=($task.State -eq 4); enabled=[bool]$task.Enabled; last_result=$task.LastTaskResult; interval=$task.Definition.Triggers.Item(1).Repetition.Interval; state=$task.State } | ConvertTo-Json -Compress
                exit 0
            }
            'enable' { $task.Enabled = $true }
            'disable' {
                $task.Enabled = $false
                # Drain an in-flight bounded modern check before an installer stops/writes.
                if ($task.Definition.Actions.Item(1).Path -ieq (Quote-Arg $launcher)) {
                    $deadline = [DateTime]::UtcNow.AddSeconds(15)
                    while ($task.State -eq 4) {
                        if ([DateTime]::UtcNow -ge $deadline) { throw 'Paused host check did not finish; mutation cancelled' }
                        Start-Sleep -Milliseconds 100
                        $task = $folder.GetTask($expected.name)
                    }
                }
            }
            'start' {
                if (-not $task.Enabled) { throw 'Owned host task is disabled; explicit start must enable it first' }
                # TASK_RUN_USE_SESSION_ID routes to this interactive caller, not another user logon.
                $null = $task.RunEx($null, 4, [System.Diagnostics.Process]::GetCurrentProcess().SessionId, $null)
            }
            'remove' { $folder.DeleteTask($expected.name, 0) }
            'remove-if-owned' { $folder.DeleteTask($expected.name, 0) }
            default { throw "Unknown task operation: $($r.op)" }
        }
    }
    'null'
} catch {
    [Console]::Error.WriteLine($_.Exception.Message)
    exit 1
}
