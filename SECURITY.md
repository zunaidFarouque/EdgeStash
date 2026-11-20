# EdgeStash Security Policy

EdgeStash is designed with a defense-in-depth security architecture that adheres to the Principle of Least Privilege on Microsoft Windows.

## Least-Privilege Execution Model (`asInvoker`)

Unlike many legacy window managers and desktop enhancement tools, EdgeStash:
- Runs strictly with standard user privileges (`asInvoker` requested execution level).
- Never requires administrative elevation (UAC prompt) for normal operation.
- Respects Windows User Interface Privilege Isolation (UIPI): EdgeStash cannot and will not interact with elevated or secure system dialogs unless explicitly running in a matching integrity level.
- Communicates between the GUI, CLI, and background daemon via named pipes secured with local access-control lists.

## Reporting a Security Concern

If you discover a security vulnerability or privilege boundary concern in EdgeStash, please report it responsibly:

1. **Email Contact**: Send details to `zunaidfarouque@gmail.com`.
2. **Details to Include**:
   - A description of the issue and attack vector.
   - Proof of concept code or clear reproduction steps.
   - Affected Windows version and EdgeStash release tag.
3. **Response Timeline**:
   - Initial acknowledgement within 48 hours.
   - An assessment and timeline for a patch within 5 business days.

Please do not open public GitHub issues for undisclosed security vulnerabilities.
