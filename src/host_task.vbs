' arTerm owned hidden periodic host check v1
Option Explicit
Dim args, shell, command, result
Set args = WScript.Arguments
If args.Count = 1 Then
    If args(0) = "--probe" Then WScript.Quit 0
End If
If args.Count <> 2 Then WScript.Quit 2
Function Quoted(value)
    Dim trailing
    If InStr(value, Chr(34)) > 0 Or InStr(value, "%") > 0 Then WScript.Quit 2
    trailing = 0
    Do While trailing < Len(value)
        If Mid(value, Len(value) - trailing, 1) <> "\" Then Exit Do
        trailing = trailing + 1
    Loop
    Quoted = Chr(34) & value & String(trailing, "\") & Chr(34)
End Function
Set shell = CreateObject("WScript.Shell")
command = Quoted(args(0)) & " ensure-running --data-root " & Quoted(args(1))
result = shell.Run(command, 0, True)
WScript.Quit result
